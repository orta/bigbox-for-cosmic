// SPDX-License-Identifier: MPL-2.0

use crate::config::{Config, Friend};
use crate::fl;
use bigbox_for_cosmic::api::{self, ApiError, Attendee, ClassEvent, Directory, Profile};
use bigbox_for_cosmic::categories::{self, Category};
use chrono::{Duration, Local, NaiveDate};
use cosmic::app::context_drawer;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::{Alignment, Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, about::About, icon, menu, nav_bar};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] = include_bytes!("../resources/icons/hicolor/scalable/apps/icon.svg");

/// How many days of planning the day picker offers. The API will happily serve
/// months ahead, but classes that far out are rarely bookable yet.
const DAYS_AHEAD: i64 = 14;

/// Where a friend's account has got to. Signing in as someone else can fail on
/// its own (wrong password, their session revoked) without affecting your own,
/// so each is tracked separately and reported per friend.
#[derive(Debug, Clone, Default)]
pub enum FriendState {
    #[default]
    Idle,
    SigningIn,
    /// Class IRIs this friend holds a live booking for.
    Loaded(HashSet<String>),
    Failed(String),
}

/// The application model stores app-specific state.
pub struct AppModel {
    /// Application state managed by the COSMIC runtime.
    core: cosmic::Core,
    /// Display a context drawer with the designated page if defined.
    context_page: ContextPage,
    /// The about page for this app.
    about: About,
    /// Contains items assigned to the nav bar panel.
    nav: nav_bar::Model,
    /// Key bindings for the application's menu bar.
    key_binds: HashMap<menu::KeyBind, MenuAction>,
    /// Configuration data that persists between application runs.
    config: Config,
    /// Handle for writing config to disk.
    config_handler: Option<cosmic_config::Config>,

    // --- Session ---
    /// `None` until sign-in succeeds. Shared with the async tasks that use it,
    /// which is why it's behind an `Arc` rather than cloned per request.
    client: Option<Arc<api::Client>>,
    profile: Option<Profile>,
    /// Lookup tables for turning a class's IRIs into names.
    directory: Directory,

    // --- Sign-in form (transient) ---
    login_email: String,
    login_password: String,
    login_error: Option<String>,
    signing_in: bool,

    // --- Planning ---
    selected_day: NaiveDate,
    /// Classes per day, cached so flicking between days doesn't refetch.
    days: HashMap<NaiveDate, Vec<ClassEvent>>,
    /// Days with a fetch in flight, so the view can show progress per day and
    /// the app doesn't stack duplicate requests.
    loading_days: HashSet<NaiveDate>,
    planning_error: Option<String>,
    /// Free-text filter over class name, tags and category — "legs", "weights".
    filter: String,
    /// Group the day under category headings rather than a flat time-ordered
    /// list. On by default; that's the point of the categories.
    group_by_category: bool,

    // --- Bookings ---
    bookings: Vec<Attendee>,
    bookings_error: Option<String>,
    loading_bookings: bool,

    // --- Friends (indexed alongside `config.friends`) ---
    friend_states: Vec<FriendState>,
    friend_form: Friend,
    friend_form_error: Option<String>,

    // --- In-flight actions ---
    /// Class IRIs with a book or cancel in flight, keyed by class so the right
    /// row can show its own progress.
    pending: HashSet<String>,
    /// Transient confirmation of the last completed booking action.
    status: Option<String>,
}

/// Messages emitted by the application and its widgets.
#[derive(Debug, Clone)]
pub enum Message {
    // Sign-in
    LoginEmailChanged(String),
    LoginPasswordChanged(String),
    SubmitLogin,
    SignedIn(Box<Result<Arc<api::Client>, ApiError>>),
    SignOut,

    // Data
    DirectoryLoaded(Box<Result<Directory, ApiError>>),
    DayLoaded(NaiveDate, Box<Result<Vec<ClassEvent>, ApiError>>),
    BookingsLoaded(Box<Result<Vec<Attendee>, ApiError>>),
    SelectDay(NaiveDate),
    FilterChanged(String),
    ToggleGrouping,
    Refresh,

    // Booking
    Book(String),
    /// The attendee to cancel, plus the class it belongs to so the row it came
    /// from can be un-marked when it finishes.
    Cancel(String, String),
    ActionDone(String, Box<Result<String, ApiError>>),

    // Friends
    FriendNameChanged(String),
    FriendEmailChanged(String),
    FriendPasswordChanged(String),
    AddFriend,
    RemoveFriend(usize),
    FriendSignedIn(usize, Box<Result<Arc<api::Client>, ApiError>>),
    FriendBookingsLoaded(usize, Box<Result<Vec<Attendee>, ApiError>>),

    // Chrome
    LaunchUrl(String),
    ToggleContextPage(ContextPage),
    UpdateConfig(Config),
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.orta.BigboxForCosmic";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let mut nav = nav_bar::Model::default();

        nav.insert()
            .text(fl!("planning"))
            .data::<Page>(Page::Planning)
            .icon(icon::from_name("x-office-calendar-symbolic"))
            .activate();

        nav.insert()
            .text(fl!("my-bookings"))
            .data::<Page>(Page::Bookings)
            .icon(icon::from_name("object-select-symbolic"));

        nav.insert()
            .text(fl!("friends"))
            .data::<Page>(Page::Friends)
            .icon(icon::from_name("system-users-symbolic"));

        let about = About::default()
            .name(fl!("app-title"))
            .icon(widget::icon::from_svg_bytes(APP_ICON))
            .version(env!("CARGO_PKG_VERSION"))
            .links([(fl!("repository"), REPOSITORY)])
            .license(env!("CARGO_PKG_LICENSE"));

        let config_handler = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).ok();
        let config = config_handler
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();

        let mut app = AppModel {
            core,
            context_page: ContextPage::default(),
            about,
            nav,
            key_binds: HashMap::new(),
            login_email: config.email.clone(),
            login_password: config.password.clone(),
            friend_states: vec![FriendState::Idle; config.friends.len()],
            config,
            config_handler,
            client: None,
            profile: None,
            directory: Directory::default(),
            login_error: None,
            signing_in: false,
            selected_day: Local::now().date_naive(),
            days: HashMap::new(),
            loading_days: HashSet::new(),
            planning_error: None,
            filter: String::new(),
            group_by_category: true,
            bookings: Vec::new(),
            bookings_error: None,
            loading_bookings: false,
            friend_form: Friend::default(),
            friend_form_error: None,
            pending: HashSet::new(),
            status: None,
        };

        // Saved credentials mean the user never has to see the sign-in form.
        let login_task = if app.config.has_credentials() {
            app.signing_in = true;
            app.sign_in()
        } else {
            Task::none()
        };

        let title_task = app.update_title();

        (app, Task::batch([login_task, title_task]))
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        let menu_bar = menu::bar(vec![menu::Tree::with_children(
            menu::root(fl!("view")).apply(Element::from),
            menu::items(
                &self.key_binds,
                vec![menu::Item::Button(fl!("about"), None, MenuAction::About)],
            ),
        )]);

        vec![menu_bar.into()]
    }

    /// The nav bar is only useful once there's an account to browse.
    fn nav_model(&self) -> Option<&nav_bar::Model> {
        self.client.as_ref().map(|_| &self.nav)
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Self::Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match self.context_page {
            ContextPage::About => context_drawer::about(
                &self.about,
                |url| Message::LaunchUrl(url.to_string()),
                Message::ToggleContextPage(ContextPage::About),
            ),
        })
    }

    fn view(&self) -> Element<'_, Self::Message> {
        if self.client.is_none() {
            return self.view_sign_in();
        }

        match self.nav.active_data::<Page>() {
            Some(Page::Bookings) => self.view_bookings(),
            Some(Page::Friends) => self.view_friends(),
            _ => self.view_planning(),
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch(vec![
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
        ])
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::LoginEmailChanged(email) => {
                self.login_email = email;
            }

            Message::LoginPasswordChanged(password) => {
                self.login_password = password;
            }

            Message::SubmitLogin => {
                if self.login_email.is_empty() || self.login_password.is_empty() {
                    return Task::none();
                }
                self.signing_in = true;
                self.login_error = None;
                return self.sign_in();
            }

            Message::SignedIn(result) => {
                self.signing_in = false;
                match *result {
                    Ok(client) => {
                        // Only persist once the server has actually accepted
                        // them, so a typo doesn't get saved.
                        self.save_credentials();
                        self.profile = Some(client.profile().clone());
                        self.client = Some(client);
                        self.login_error = None;
                        return Task::batch([
                            self.fetch_directory(),
                            self.fetch_day(self.selected_day),
                            self.fetch_bookings(),
                            self.sign_in_friends(),
                            self.update_title(),
                        ]);
                    }
                    Err(error) => {
                        self.login_error = Some(error.to_string());
                    }
                }
            }

            Message::SignOut => {
                self.client = None;
                self.profile = None;
                self.directory = Directory::default();
                self.days.clear();
                self.bookings.clear();
                self.login_password.clear();
                self.login_error = None;
                self.status = None;
                self.config.password.clear();
                // Friends' sessions are separate logins; drop them too rather
                // than leave someone else's data on screen.
                self.friend_states = vec![FriendState::Idle; self.config.friends.len()];
                self.save_config();
                return self.update_title();
            }

            Message::DirectoryLoaded(result) => match *result {
                Ok(directory) => self.directory = directory,
                Err(error) => return self.handle_error(error, |app, msg| app.planning_error = msg),
            },

            Message::DayLoaded(day, result) => {
                self.loading_days.remove(&day);
                match *result {
                    Ok(events) => {
                        self.days.insert(day, events);
                        self.planning_error = None;
                    }
                    Err(error) => {
                        return self.handle_error(error, |app, msg| app.planning_error = msg);
                    }
                }
            }

            Message::BookingsLoaded(result) => {
                self.loading_bookings = false;
                match *result {
                    Ok(bookings) => {
                        self.bookings = bookings;
                        self.bookings_error = None;
                    }
                    Err(error) => {
                        return self.handle_error(error, |app, msg| app.bookings_error = msg);
                    }
                }
            }

            Message::SelectDay(day) => {
                self.selected_day = day;
                self.status = None;
                if !self.days.contains_key(&day) {
                    return self.fetch_day(day);
                }
            }

            Message::FilterChanged(filter) => {
                self.filter = filter;
            }

            Message::ToggleGrouping => {
                self.group_by_category = !self.group_by_category;
            }

            Message::Refresh => {
                self.status = None;
                self.days.remove(&self.selected_day);
                return Task::batch([
                    self.fetch_day(self.selected_day),
                    self.fetch_bookings(),
                    self.sign_in_friends(),
                ]);
            }

            Message::Book(class_id) => {
                let Some(client) = self.client.clone() else {
                    return Task::none();
                };
                self.pending.insert(class_id.clone());
                self.status = None;
                let key = class_id.clone();
                return Task::perform(
                    async move {
                        // The same request yields either a seat or a queue
                        // place, so the confirmation comes from what came back.
                        client.book(&class_id).await.map(|attendee| {
                            if attendee.is_queued() {
                                fl!("queued-confirmation")
                            } else {
                                fl!("booked-confirmation")
                            }
                        })
                    },
                    move |result| {
                        cosmic::Action::App(Message::ActionDone(key.clone(), Box::new(result)))
                    },
                );
            }

            Message::Cancel(attendee_id, class_id) => {
                let Some(client) = self.client.clone() else {
                    return Task::none();
                };
                self.pending.insert(class_id.clone());
                self.status = None;
                return Task::perform(
                    async move {
                        client
                            .cancel(&attendee_id)
                            .await
                            .map(|()| fl!("cancelled-confirmation"))
                    },
                    move |result| {
                        cosmic::Action::App(Message::ActionDone(class_id.clone(), Box::new(result)))
                    },
                );
            }

            Message::ActionDone(class_id, result) => {
                self.pending.remove(&class_id);
                match *result {
                    Ok(confirmation) => {
                        self.status = Some(confirmation);
                        // Places remaining and the member's own attendee record
                        // both changed server-side, so re-read rather than
                        // patching the cached copy.
                        self.days.remove(&self.selected_day);
                        return Task::batch([
                            self.fetch_day(self.selected_day),
                            self.fetch_bookings(),
                        ]);
                    }
                    Err(error) => {
                        return self.handle_error(error, |app, msg| app.planning_error = msg);
                    }
                }
            }

            Message::FriendNameChanged(name) => self.friend_form.name = name,
            Message::FriendEmailChanged(email) => self.friend_form.email = email,
            Message::FriendPasswordChanged(password) => self.friend_form.password = password,

            Message::AddFriend => {
                let friend = std::mem::take(&mut self.friend_form);
                if friend.email.trim().is_empty() || friend.password.is_empty() {
                    self.friend_form_error = Some(fl!("friend-needs-credentials"));
                    self.friend_form = friend;
                    return Task::none();
                }
                if self
                    .config
                    .friends
                    .iter()
                    .any(|existing| existing.email.eq_ignore_ascii_case(&friend.email))
                {
                    self.friend_form_error = Some(fl!("friend-already-added"));
                    self.friend_form = friend;
                    return Task::none();
                }

                self.friend_form_error = None;
                self.config.friends.push(friend);
                self.friend_states.push(FriendState::Idle);
                self.save_config();
                return self.sign_in_friend(self.config.friends.len() - 1);
            }

            Message::RemoveFriend(index) => {
                if index < self.config.friends.len() {
                    self.config.friends.remove(index);
                    self.friend_states.remove(index);
                    self.save_config();
                }
            }

            Message::FriendSignedIn(index, result) => match *result {
                Ok(client) => return self.fetch_friend_bookings(index, client),
                Err(error) => {
                    if let Some(state) = self.friend_states.get_mut(index) {
                        *state = FriendState::Failed(error.to_string());
                    }
                }
            },

            Message::FriendBookingsLoaded(index, result) => {
                let Some(state) = self.friend_states.get_mut(index) else {
                    return Task::none();
                };
                let now = Local::now().naive_local();
                *state = match *result {
                    Ok(bookings) => FriendState::Loaded(api::upcoming_class_ids(bookings, now)),
                    Err(error) => FriendState::Failed(error.to_string()),
                };
            }

            Message::ToggleContextPage(context_page) => {
                if self.context_page == context_page {
                    self.core.window.show_context = !self.core.window.show_context;
                } else {
                    self.context_page = context_page;
                    self.core.window.show_context = true;
                }
            }

            Message::UpdateConfig(config) => {
                // Keep the parallel friend state in step with whatever the
                // config now says, or later indexing goes out of bounds.
                self.friend_states
                    .resize(config.friends.len(), FriendState::Idle);
                self.config = config;
            }

            Message::LaunchUrl(url) => {
                if let Err(err) = open::that_detached(&url) {
                    eprintln!("failed to open {url:?}: {err}");
                }
            }
        }

        Task::none()
    }

    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Self::Message>> {
        self.nav.activate(id);

        // The bookings list is the one page that can go stale without the user
        // touching this app — they might have booked on their phone.
        let refresh = match self.nav.active_data::<Page>() {
            Some(Page::Bookings) => self.fetch_bookings(),
            _ => Task::none(),
        };

        Task::batch([refresh, self.update_title()])
    }
}

impl AppModel {
    // --- Tasks ---

    fn sign_in(&self) -> Task<cosmic::Action<Message>> {
        let email = self.login_email.clone();
        let password = self.login_password.clone();
        Task::perform(
            async move { api::Client::login(&email, &password).await.map(Arc::new) },
            |result| cosmic::Action::App(Message::SignedIn(Box::new(result))),
        )
    }

    fn sign_in_friends(&mut self) -> Task<cosmic::Action<Message>> {
        let tasks: Vec<_> = (0..self.config.friends.len())
            .map(|index| self.sign_in_friend(index))
            .collect();
        Task::batch(tasks)
    }

    fn sign_in_friend(&mut self, index: usize) -> Task<cosmic::Action<Message>> {
        let Some(friend) = self.config.friends.get(index) else {
            return Task::none();
        };
        let (email, password) = (friend.email.clone(), friend.password.clone());

        if let Some(state) = self.friend_states.get_mut(index) {
            *state = FriendState::SigningIn;
        }

        Task::perform(
            async move { api::Client::login(&email, &password).await.map(Arc::new) },
            move |result| cosmic::Action::App(Message::FriendSignedIn(index, Box::new(result))),
        )
    }

    /// Friends' clients are used once, to read their bookings, then dropped —
    /// there's nothing else the app does on their behalf, so there's no reason
    /// to keep someone else's session alive in memory.
    fn fetch_friend_bookings(
        &self,
        index: usize,
        client: Arc<api::Client>,
    ) -> Task<cosmic::Action<Message>> {
        let today = Local::now().date_naive();
        Task::perform(
            async move { client.bookings_from(today).await },
            move |result| {
                cosmic::Action::App(Message::FriendBookingsLoaded(index, Box::new(result)))
            },
        )
    }

    fn fetch_directory(&self) -> Task<cosmic::Action<Message>> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        Task::perform(async move { client.directory().await }, |result| {
            cosmic::Action::App(Message::DirectoryLoaded(Box::new(result)))
        })
    }

    fn fetch_day(&mut self, day: NaiveDate) -> Task<cosmic::Action<Message>> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let Some(club_id) = self.profile.as_ref().map(|p| p.club_id.clone()) else {
            return Task::none();
        };
        if !self.loading_days.insert(day) {
            // Already in flight.
            return Task::none();
        }

        Task::perform(
            async move {
                client
                    .planning(&club_id, day, day + Duration::days(1))
                    .await
            },
            move |result| cosmic::Action::App(Message::DayLoaded(day, Box::new(result))),
        )
    }

    fn fetch_bookings(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        self.loading_bookings = true;
        // Only what's still to come — the page filters to future classes
        // anyway, and a long-standing member's full history runs to many pages.
        let today = Local::now().date_naive();
        Task::perform(async move { client.bookings_from(today).await }, |result| {
            cosmic::Action::App(Message::BookingsLoaded(Box::new(result)))
        })
    }

    /// Routes an API error either to the sign-in screen (when the session is
    /// gone) or to an in-page message via `set`.
    fn handle_error(
        &mut self,
        error: ApiError,
        set: fn(&mut Self, Option<String>),
    ) -> Task<cosmic::Action<Message>> {
        if matches!(error, ApiError::SessionExpired) {
            self.client = None;
            self.profile = None;
            self.login_error = Some(fl!("session-expired"));
            // The stored password is probably still good — a refresh token can
            // expire on its own — so try once to get back in silently.
            if self.config.has_credentials() {
                self.signing_in = true;
                return self.sign_in();
            }
            return Task::none();
        }

        set(self, Some(error.to_string()));
        Task::none()
    }

    // --- Config ---

    fn save_credentials(&mut self) {
        self.config.email = self.login_email.clone();
        self.config.password = self.login_password.clone();
        self.save_config();
    }

    fn save_config(&mut self) {
        let Some(handler) = self.config_handler.as_ref() else {
            return;
        };
        if let Err(why) = self.config.write_entry(handler) {
            eprintln!("failed to save config: {why}");
        }
    }

    pub fn update_title(&mut self) -> Task<cosmic::Action<Message>> {
        let mut window_title = fl!("app-title");

        if self.client.is_some()
            && let Some(page) = self.nav.text(self.nav.active())
        {
            window_title.push_str(" — ");
            window_title.push_str(page);
        }

        match self.core.main_window_id() {
            Some(id) => self.set_window_title(window_title, id),
            None => Task::none(),
        }
    }

    /// Names of friends holding a live booking on this class.
    fn friends_attending(&self, class_id: &str) -> Vec<&str> {
        self.config
            .friends
            .iter()
            .zip(&self.friend_states)
            .filter_map(|(friend, state)| match state {
                FriendState::Loaded(classes) if classes.contains(class_id) => {
                    Some(friend.display_name())
                }
                _ => None,
            })
            .collect()
    }

    // --- Views ---

    fn view_sign_in(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();

        let mut col = widget::column::with_capacity(7)
            .spacing(spacing.space_m)
            .max_width(380.0)
            .align_x(Horizontal::Center);

        col = col
            .push(widget::text::title2(fl!("app-title")))
            .push(widget::text(fl!("sign-in-prompt")));

        col = col.push(
            widget::text_input(fl!("email"), &self.login_email)
                .on_input(Message::LoginEmailChanged),
        );

        col = col.push(
            widget::secure_input(fl!("password"), &self.login_password, None::<Message>, true)
                .on_input(Message::LoginPasswordChanged)
                .on_submit(|_| Message::SubmitLogin),
        );

        if let Some(error) = &self.login_error {
            col = col.push(widget::text(error.as_str()));
        }

        if self.signing_in {
            col = col.push(widget::text(fl!("signing-in")));
        } else {
            let can_submit = !self.login_email.is_empty() && !self.login_password.is_empty();
            let mut button = widget::button::suggested(fl!("sign-in"));
            if can_submit {
                button = button.on_press(Message::SubmitLogin);
            }
            col = col.push(button);
        }

        widget::container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .into()
    }

    fn view_planning(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut content = widget::column::with_capacity(6).spacing(spacing.space_m);

        content = content.push(self.view_header(fl!("planning")));
        content = content.push(self.view_day_picker());
        content = content.push(self.view_filter_row());

        if let Some(status) = &self.status {
            content = content.push(widget::text(status.as_str()));
        }

        if let Some(error) = &self.planning_error {
            content = content.push(widget::text(error.as_str()));
        }

        if self.loading_days.contains(&self.selected_day) {
            content = content.push(widget::text(fl!("loading-classes")));
            return scroll(content, spacing.space_m as f32);
        }

        let Some(events) = self.days.get(&self.selected_day) else {
            content = content.push(widget::text(fl!("loading-classes")));
            return scroll(content, spacing.space_m as f32);
        };

        let matching: Vec<&ClassEvent> = events
            .iter()
            .filter(|event| {
                let name = self.directory.activity_name(event).unwrap_or("");
                categories::matches(name, &self.filter)
            })
            .collect();

        if matching.is_empty() {
            let message = if self.filter.trim().is_empty() {
                fl!("no-classes")
            } else {
                fl!("no-classes-matching", query = self.filter.trim())
            };
            content = content.push(widget::text(message));
            return scroll(content, spacing.space_m as f32);
        }

        if self.group_by_category {
            for category in Category::all() {
                let in_category: Vec<&ClassEvent> = matching
                    .iter()
                    .copied()
                    .filter(|event| {
                        let name = self.directory.activity_name(event).unwrap_or("");
                        categories::lookup(name).category == *category
                    })
                    .collect();

                if in_category.is_empty() {
                    continue;
                }

                // Heading and explainer sit above the list rather than inside
                // the section's own title, which has no room for a subtitle.
                content = content.push(
                    widget::column::with_capacity(2)
                        .spacing(spacing.space_xxxs)
                        .push(widget::text::title4(format!(
                            "{}  ({})",
                            category.label(),
                            in_category.len()
                        )))
                        .push(widget::text::caption(category.description())),
                );
                content = content.push(self.view_class_section(&in_category, None));
            }
        } else {
            content = content.push(self.view_class_section(&matching, None));
        }

        scroll(content, spacing.space_m as f32)
    }

    fn view_header(&self, title: String) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();

        let club = self
            .profile
            .as_ref()
            .and_then(|p| self.directory.clubs.get(&p.club_id))
            .and_then(|club| club.name.as_deref());

        let mut heading = widget::column::with_capacity(2).push(widget::text::title3(title));
        if let Some(club) = club {
            heading = heading.push(widget::text::caption(club));
        }

        widget::row::with_capacity(4)
            .push(heading)
            .push(widget::space::horizontal())
            .push(widget::button::text(fl!("refresh")).on_press(Message::Refresh))
            .push(widget::button::text(fl!("sign-out")).on_press(Message::SignOut))
            .align_y(Alignment::Center)
            .spacing(spacing.space_xs)
            .into()
    }

    /// A strip of day buttons, the selected one highlighted.
    fn view_day_picker(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let today = Local::now().date_naive();

        let mut row = widget::row::with_capacity(DAYS_AHEAD as usize).spacing(spacing.space_xxs);

        for offset in 0..DAYS_AHEAD {
            let day = today + Duration::days(offset);
            let label = if day == today {
                fl!("today")
            } else {
                day.format("%a %-d").to_string()
            };

            let button = if day == self.selected_day {
                widget::button::suggested(label)
            } else {
                widget::button::text(label)
            };

            row = row.push(button.on_press(Message::SelectDay(day)));
        }

        // Horizontal, so two weeks of days stay on one line on a narrow window.
        //
        // The scrollbar is drawn along the bottom edge of the viewport, and
        // unlike `scrollable::vertical` the horizontal constructor reserves no
        // room for it — so without this the bar sits on top of the day labels.
        // Padding the *content* (not the scrollable) is what grows the viewport
        // and gives the bar somewhere of its own to live.
        let track = spacing.space_xxs + spacing.space_xxxs;
        widget::scrollable::horizontal(widget::container(row).padding(cosmic::iced::Padding {
            bottom: f32::from(track),
            ..Default::default()
        }))
        .width(Length::Fill)
        .into()
    }

    /// Search box plus the grouping toggle.
    fn view_filter_row(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();

        let toggle_label = if self.group_by_category {
            fl!("view-by-time")
        } else {
            fl!("view-by-category")
        };

        widget::row::with_capacity(2)
            .push(
                widget::search_input(fl!("filter-placeholder"), &self.filter)
                    .on_input(Message::FilterChanged)
                    .on_clear(Message::FilterChanged(String::new()))
                    .width(Length::Fill),
            )
            .push(widget::button::text(toggle_label).on_press(Message::ToggleGrouping))
            .align_y(Alignment::Center)
            .spacing(spacing.space_xs)
            .into()
    }

    /// One list of classes, optionally under a category heading.
    fn view_class_section(
        &self,
        events: &[&ClassEvent],
        title: Option<String>,
    ) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let contact_id = self
            .profile
            .as_ref()
            .map(|p| p.contact_id.as_str())
            .unwrap_or_default();

        let mut section = widget::settings::section::with_column(
            widget::list_column()
                .list_item_padding([spacing.space_xxs, 0])
                .divider_padding(0),
        );
        if let Some(title) = title {
            section = section.title(title);
        }

        let now = Local::now().naive_local();

        for event in events {
            let entry = self.directory.resolve(event, contact_id);

            let mut details = vec![entry.places_text()];
            if let Some(coach) = &entry.coach {
                details.push(coach.clone());
            }
            if let Some(studio) = &entry.studio {
                details.push(studio.clone());
            }
            details.retain(|part| !part.is_empty());

            let mut description = details.join(" · ");

            if entry.queued {
                description.push('\n');
                description.push_str(&match entry.queue_position {
                    Some(position) => fl!("waiting-list-position", position = position),
                    None => fl!("waiting-list-joined"),
                });
            }

            let friends = self.friends_attending(&entry.id);
            if !friends.is_empty() {
                description.push('\n');
                description.push_str(&fl!("friends-going", names = friends.join(", ")));
            }

            let title = format!("{}  {}", entry.time_range(), entry.name);
            let started = entry.start.is_some_and(|start| start < now);

            // A class that's already started can't be acted on, so it's dimmed
            // rather than hidden — you still want to see that you went to the
            // 07:00, and where the day's gaps were.
            section = section.add(if started {
                widget::settings::item_row(vec![
                    widget::column::with_capacity(2)
                        .spacing(2)
                        .push(widget::text::body(title).class(cosmic::theme::Text::Custom(dimmed)))
                        .push(
                            widget::text::caption(description)
                                .class(cosmic::theme::Text::Custom(dimmed)),
                        )
                        .into(),
                    widget::space::horizontal().into(),
                    widget::text::caption(fl!("class-finished"))
                        .class(cosmic::theme::Text::Custom(dimmed))
                        .into(),
                ])
            } else {
                widget::settings::item::builder(title)
                    .description(description)
                    .control(self.view_action(&entry))
            });
        }

        section.into()
    }

    /// The book / cancel control for one class.
    fn view_action(&self, entry: &api::PlanningEntry) -> Element<'_, Message> {
        if self.pending.contains(&entry.id) {
            return widget::text(fl!("working")).into();
        }

        if entry.is_cancelled {
            return widget::text(fl!("class-cancelled")).into();
        }

        // Queued first: a waiting-list place also makes `is_full` true, so
        // checking fullness ahead of this would hide the member's own place.
        if entry.booked || entry.queued {
            return match &entry.attendee_id {
                Some(attendee_id) => widget::button::destructive(fl!("cancel"))
                    .on_press(Message::Cancel(attendee_id.clone(), entry.id.clone()))
                    .into(),
                None => widget::text(fl!("booked")).into(),
            };
        }

        if entry.is_full {
            // Joining the waiting list is the same request as booking — the
            // server hands out a queue place when the seats are gone.
            if entry.has_queue_space {
                return widget::button::standard(fl!("join-waiting-list"))
                    .on_press(Message::Book(entry.id.clone()))
                    .into();
            }
            return widget::text(fl!("full")).into();
        }

        widget::button::suggested(fl!("book"))
            .on_press(Message::Book(entry.id.clone()))
            .into()
    }

    fn view_bookings(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut content = widget::column::with_capacity(4).spacing(spacing.space_m);

        content = content.push(self.view_header(fl!("my-bookings")));

        if let Some(status) = &self.status {
            content = content.push(widget::text(status.as_str()));
        }

        if let Some(error) = &self.bookings_error {
            content = content.push(widget::text(error.as_str()));
        }

        if self.loading_bookings && self.bookings.is_empty() {
            content = content.push(widget::text(fl!("loading-bookings")));
            return scroll(content, spacing.space_m as f32);
        }

        let now = Local::now().naive_local();
        let mut upcoming: Vec<&Attendee> = self
            .bookings
            .iter()
            .filter(|attendee| attendee.is_active())
            .filter(|attendee| {
                attendee
                    .class_event
                    .as_ref()
                    .and_then(|class| class.start())
                    .is_some_and(|start| start >= now)
            })
            .collect();
        upcoming.sort_by_key(|a| a.class_event.as_ref().and_then(|c| c.start()));

        if upcoming.is_empty() {
            content = content.push(widget::text(fl!("no-bookings")));
            return scroll(content, spacing.space_m as f32);
        }

        let mut section = widget::settings::section::with_column(
            widget::list_column()
                .list_item_padding([spacing.space_xxs, 0])
                .divider_padding(0),
        );

        for attendee in upcoming {
            let Some(class) = attendee.class_event.as_deref() else {
                continue;
            };

            let name = self.directory.activity_name(class).unwrap_or("Class");
            let when = class
                .start()
                .map(|start| start.format("%a %-d %b, %H:%M").to_string())
                .unwrap_or_default();

            let mut details = vec![categories::lookup(name).category.label().to_string()];
            if attendee.is_queued() {
                details.push(fl!("waiting-list"));
            }
            if let Some(coach) = self.directory.coach_name(class) {
                details.push(coach);
            }
            if let Some(studio) = self.directory.studio_name(class) {
                details.push(studio.to_string());
            }

            let mut description = details.join(" · ");
            let friends = self.friends_attending(&class.id);
            if !friends.is_empty() {
                description.push('\n');
                description.push_str(&fl!("friends-going", names = friends.join(", ")));
            }

            let control: Element<'_, Message> = if self.pending.contains(&class.id) {
                widget::text(fl!("working")).into()
            } else if attendee.is_cancellable() {
                widget::button::destructive(fl!("cancel"))
                    .on_press(Message::Cancel(attendee.id.clone(), class.id.clone()))
                    .into()
            } else {
                // Past the club's cancellation deadline.
                widget::text(fl!("cancellation-closed")).into()
            };

            section = section.add(
                widget::settings::item::builder(format!("{when}  {name}"))
                    .description(description)
                    .control(control),
            );
        }

        content = content.push(section);
        scroll(content, spacing.space_m as f32)
    }

    fn view_friends(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut content = widget::column::with_capacity(5).spacing(spacing.space_m);

        content = content.push(self.view_header(fl!("friends")));
        content = content.push(widget::text(fl!("friends-explainer")));

        if !self.config.friends.is_empty() {
            let mut section = widget::settings::section().title(fl!("friends-added"));

            for (index, friend) in self.config.friends.iter().enumerate() {
                let status = match self.friend_states.get(index) {
                    Some(FriendState::SigningIn) => fl!("signing-in"),
                    Some(FriendState::Loaded(classes)) => {
                        fl!("friend-booking-count", count = classes.len())
                    }
                    Some(FriendState::Failed(error)) => error.clone(),
                    _ => fl!("friend-not-loaded"),
                };

                section = section.add(
                    widget::settings::item::builder(friend.display_name().to_string())
                        .description(format!("{} · {status}", friend.email))
                        .control(
                            widget::button::destructive(fl!("remove"))
                                .on_press(Message::RemoveFriend(index)),
                        ),
                );
            }

            content = content.push(section);
        }

        let mut form = widget::column::with_capacity(5).spacing(spacing.space_s);
        form = form
            .push(widget::text::title4(fl!("add-friend")))
            .push(
                widget::text_input(fl!("friend-name"), &self.friend_form.name)
                    .on_input(Message::FriendNameChanged),
            )
            .push(
                widget::text_input(fl!("email"), &self.friend_form.email)
                    .on_input(Message::FriendEmailChanged),
            )
            .push(
                widget::secure_input(
                    fl!("password"),
                    &self.friend_form.password,
                    None::<Message>,
                    true,
                )
                .on_input(Message::FriendPasswordChanged)
                .on_submit(|_| Message::AddFriend),
            );

        if let Some(error) = &self.friend_form_error {
            form = form.push(widget::text(error.as_str()));
        }

        form = form.push(widget::button::suggested(fl!("add-friend")).on_press(Message::AddFriend));

        content = content.push(form);
        scroll(content, spacing.space_m as f32)
    }
}

/// Text style for classes that have already started: the theme's normal
/// foreground at half alpha.
///
/// Resolved from the live theme rather than a fixed colour so it stays legible
/// in both light and dark mode.
fn dimmed(theme: &cosmic::Theme) -> cosmic::iced::advanced::widget::text::Style {
    let cosmic = theme.cosmic();
    let mut color = cosmic.on_bg_color();
    color.alpha *= 0.5;

    cosmic::iced::advanced::widget::text::Style {
        color: Some(color.into()),
        selected_fill: cosmic.accent.base.into(),
    }
}

/// Wraps a page body in a scrollable with room for the scrollbar.
fn scroll(
    content: widget::Column<'_, Message, cosmic::Theme>,
    padding: f32,
) -> Element<'_, Message> {
    widget::scrollable(widget::container(content).padding(cosmic::iced::Padding {
        right: padding,
        ..Default::default()
    }))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The page to display in the application.
pub enum Page {
    Planning,
    Bookings,
    Friends,
}

/// The context page to display in the context drawer.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ContextPage {
    #[default]
    About,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    About,
}

impl menu::action::MenuAction for MenuAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match self {
            MenuAction::About => Message::ToggleContextPage(ContextPage::About),
        }
    }
}
