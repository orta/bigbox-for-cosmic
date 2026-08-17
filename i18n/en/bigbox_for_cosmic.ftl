app-title = BigBox
app-comment = Book classes at BigBox Leisure Club
app-keywords = gym;fitness;classes;booking;
about = About
repository = Repository
view = View
settings = Settings
git-description = Git commit {$hash} on {$date}

## Pages

planning = Planning
my-bookings = My Bookings
friends = Friends

## Sign in

sign-in = Sign In
sign-in-prompt = Sign in with your BigBox Leisure Club account
signing-in = Signing in…
sign-out = Sign Out
email = Email address
password = Password
session-expired = Your session expired — signing back in…

## Planning

today = Today
refresh = Refresh
loading-classes = Loading classes…
no-classes = No classes scheduled for this day
no-classes-matching = No classes matching "{ $query }" on this day
filter-placeholder = Search: legs, strength, weights, yoga…
view-by-time = By time
view-by-category = By category
book = Book
cancel = Cancel
booked = Booked
working = Working…
full = Full
join-waiting-list = Join waiting list
waiting-list = Waiting list
waiting-list-position = On the waiting list — number { $position } in the queue
waiting-list-joined = On the waiting list
class-cancelled = Cancelled
class-finished = Finished
booked-confirmation = Booked.
queued-confirmation = Added to the waiting list. You'll be moved up automatically if a place frees up.
cancelled-confirmation = Booking cancelled.

## Bookings

loading-bookings = Loading your bookings…
no-bookings = You have no upcoming bookings
cancellation-closed = Too late to cancel

## Friends

friends-explainer = Add a friend's BigBox login to see which classes they're going to. Their bookings show up on the planning grid alongside yours.
friends-added = Added friends
add-friend = Add friend
friend-name = Name (optional)
remove = Remove
friends-going = Also going: { $names }
friend-not-loaded = Not signed in yet
friend-needs-credentials = Enter their email address and password
friend-already-added = That account has already been added
friend-booking-count =
    { $count ->
        [one] 1 upcoming booking
       *[other] { $count } upcoming bookings
    }

## Calendar sync

calendar-sync = Calendar sync
calendar-explainer = Mirror your booked classes into a Fastmail calendar, so they show up wherever you read your calendar. Waiting-list places are added too, marked as tentative.
calendar-password = App password
calendar-password-help = Sign in over CalDAV with your Fastmail address and an app password with calendar access — not your normal login password.
calendar-get-password = Create an app password
calendar-connect = Connect
calendar-connecting = Signing in…
calendar-connected-as = Connected as { $username }
calendar-disconnect = Disconnect
calendar-choose = Calendar to sync to
calendar-none-writable = No calendars on that account can be written to
calendar-enable = Sync booked classes
calendar-sync-now = Sync now
calendar-syncing = Syncing…
calendar-synced = Calendar is up to date
calendar-sync-result = Calendar updated — { $created } added, { $updated } changed, { $removed } removed
calendar-needs-calendar = Pick a calendar to sync to
close = Close
