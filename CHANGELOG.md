# Changelog

## Unreleased

- Downloads, on a server that has them: a film's or an episode's page
  gets Download, which asks for the version (Original or one of the
  server's profiles) and the tracks (all, or a choice), and shows the
  free room. Its state then shows on the page: waiting or converting on
  the server, downloading with Pause, paused with Resume, failed with
  Try again where another try can help, or downloaded with Play and
  Delete download. Downloads, in the sidebar, lists the profile's
  downloads on this computer, with the room all of them take; Settings
  › This computer › Downloads shows the profile's by server and deletes
  them. A film two profiles download is one file, kept until both delete
  it. A downloaded film or episode plays from its file, and its page's
  Play and Resume play it too, with no server needed: when none can be
  reached, Choose a server opens Downloads for the profile used last,
  with a choice of the others that have downloads here, they play, and
  what was watched is sent when the server is back. A profile whose PIN
  is asked for on this computer shows the PIN pad in place of its
  downloads; with no server the PIN is checked here, and the same PIN
  unlocks the profile once the server is back, without asking again.
  Closing the window while downloads run asks first: Keep in background
  finishes them with the window hidden, and opening Spoolframe again
  shows it; Quit stops them, and they go on from where they got to at
  the next start.
- The SDK crates are 0.14.2. The download picker shows each version's
  size ("38.4 GB" for Original, "about 3.6 GB" for a profile) and
  whether it is ready on the server or the server converts it first,
  and with All tracks on, the footer shows the chosen version's size. The
  server now offers only the profiles this computer plays. The app also
  lists playlist covers, subtitle sync, subtitle search and what comes up in a playlist to each
  server; a server without them shows nothing new. On an item's page, a
  playlist shared with you says whose it is ("Alice’s"), as its card
  does, where the server names the owner.
- Up next shows the songs a playlist plays next, in the order they play,
  on a server that says what comes up (0.13): shuffled, the shuffled
  order; with Repeat All, round to the top; with Repeat One, the song
  playing. It changes as songs play, as modes change and as the playlist
  is edited. An older server's playlist shows as before: its own order,
  or "Songs play in a shuffled order".
- Shuffle keeps its order until it is turned off: turning it on shuffles
  the whole playlist from the song playing, Repeat All plays the same
  order again, and changing Repeat or Skip watched keeps it. Turning
  shuffle on again shuffles anew, and picking a song while shuffled
  shuffles anew from that song. On a 0.13 server, changing a mode no
  longer moves past a song heard to its end.
- Playlist covers, on a server that has them: the server's picture (a
  cover someone chose, its mosaic, Liked's and All's tiles) shows on the
  Playlists cards, Pinned in the sidebar, a playlist's page, the
  playlist rows and tabs on item and show pages, and Add to playlist.
  In the editor, whoever may edit a playlist chooses a picture (JPEG,
  PNG, WebP or HEIC, up to 20 MB) through the desktop's file chooser, or
  removes the one chosen; a picture past the limits is refused before it
  is sent.
- Search for subtitles, on a server that has subtitle search: the last
  row of a film's or an episode's subtitle picker, and the player's
  subtitle menu, open a panel of what OpenSubtitles and SubDL have, in
  a language you pick: files made for this one first, each with its
  site, HI, machine-translated or AI-made, rating and downloads; ones
  removed before as bad last. It says when a site can't be reached,
  when today's downloads are used up (naming the site, and the
  time in your local clock when they come back, as the server tells it),
  and when search is off. A result
  chosen downloads as a track for everyone on the server: on the item's
  page it is then picked, and in the player it plays from where you
  are. A downloaded track says where it came from, who downloaded it
  and when, with Remove for whoever may.
- Subtitle timing, on a server that syncs subtitles: for an external
  text subtitle that plays, the player's subtitle menu shows how it is
  timed (original, syncing, synced and by how much, uncertain, or why a
  sync failed), an offset with − and + in 0.1 s steps and Reset, who set
  it and when, and Sync to this file or Sync again and Use original
  timing. The timing is the server's, for everyone: when anyone changes
  it, the subtitle is read again and shows the new timing at once,
  without restarting the film.
- Scrub-preview frames are kept in memory only, not in the artwork cache
  on disk.
- Home, the libraries, library pages, item pages and playlists show at
  once from what was read before, and are read again behind when old: a
  page changes in place when the server's answer differs. Marking
  watched, hiding, playing, liking or changing a playlist reads the
  server again, so the change shows at once.
- Movie, show and episode pages show the cast: the first six actors, each
  a round portrait (or their initials) with the part they play; a click
  opens their items. Cast comes with the server's 0.9.0.
- A show's page says when it ran: "2022", "2022–" while it is still
  airing (an episode in the last twelve months), or "2022–2025".
- An episode's page says the day it aired, or its year where the server
  does not know the day.
- The SDK crates are 0.11.0. The app now lists the features it handles
  (playlists, Liked, last viewed, sharing by name, play history) to each
  server, as the SDK no longer turns any on by itself.
- A saved server keeps only the addresses it says it answers on, and the
  path it was last reached by: an old address that another server now
  answers on is forgotten, so it no longer stops this one being reached.
- Invites are copied as https links (PX-45): Copy link and Copy invite
  text in Preferences › Invites, and the link that only finds the server,
  give `https://spoolframe.app/i#…`; the QR code keeps the spoolframe://
  one. "Add a server" says it takes either.
- Playlists (PX-40), on a server that has them; a server without them
  shows none of what follows. A Playlists item in the sidebar opens My
  playlists, Shared with me and Server-wide, with New playlist (a name,
  the kinds it holds and, for an admin, Server-wide). Changes made on
  other devices show as they happen.
- A playlist's page lists its entries with their notes (up next, watched,
  unavailable), with Continue, Play from start, Shuffle, Repeat and Skip
  watched; an entry's menu plays from it.
- Pinned playlists show in the sidebar under Pinned, in your order: pin
  one from its menu or its page, drag to reorder, right-click to unpin.
  A pin that left because the playlist was unshared or deleted says so
  once.
- Add to playlist… is on the menus of films, shows, seasons (the
  season's own More), episodes, albums, tracks and artists, and of the
  cards that have menus. Only playlists that take the item are listed;
  read-only ones and others' server-wide ones are greyed, saying why.
  New playlist… makes one with the item's kind ticked and adds the item.
  A toast says where it went, with Undo, which takes out exactly what the
  add put in (a whole season too).
- Edit, for a playlist's owner, an editor, or an admin on a server-wide
  one: drag entries to reorder and remove them. The owner also renames
  it, adds kinds it holds, lets someone it is shared with edit or stops
  sharing with them, makes it personal (or, for an admin, server-wide),
  and deletes it. Each change is sent as it is made; the name goes with
  Done. When someone else changed the same entries, it asks to reload.
- Playing a playlist goes on through it: as each entry ends, or on Next,
  the server's next entry plays (with its shuffle, repeat and skip
  watched), from where you stopped in it. At the end it says "End of
  <playlist>", and Continue starts from the top. The player, the bar and
  Now Playing say "Playing from <playlist>", and the button over the
  credits says "Next in <playlist>". Music queued by hand plays first,
  then the playlist goes on.
- Movie, episode, album and artist pages list the playlists that hold
  them: choosing one opens the playlist, and right-click plays it. On a
  show's page each playlist holding any of its episodes is a tab after
  the seasons, past a thin divider, marked with a teal playlist icon;
  choosing it opens the playlist's page over the show's, and Back
  returns to the show on the season you were on. The tabs scroll
  sideways when they do not fit. Which playlists hold an item is read
  with the page, so a change shows the next time it opens.
- Sharing by name, on a server that has it: the editor's Add someone
  opens a picker of the people the playlist can go to, searched by name
  or username, a page at a time, each with their @username (or, for a
  portable user, that they are one) and a Local or Portable badge; a
  Can edit switch picks their access. Those it is shared with show by
  name, and playlists shared with you say whose they are ("Alice’s ·
  read only") in place of "Shared with you".
- Liked and All (PX-43), on a server that has them: hearts on an
  album's track rows, on a playlist's track entries and beside the title
  in the bar while a track plays; a tap likes or un-likes at once, and
  puts it back with a word if the server refuses. An album's page has a
  heart by Play, full when every track is liked, half when some are: a
  tap on a full one un-likes the album, on any other likes the rest.
  Playlists lists Liked and All first, under Music, with their own
  tiles, and Pinned can hold them. Neither is renamed, shared or
  deleted; Liked's Edit reorders and removes; All is read-only. Add to
  playlist shows them greyed: Liked takes tracks by their hearts, and
  All has every track already.
- Play history (PX-52), on a server that keeps it: Home has a
  "Recently played music" row, the albums of the tracks played lately on
  any device, newest first. Movie and episode pages say how often they
  were played and when last ("Played 3 times · last 2 days ago"), and an
  album's track rows say it on hover. An item viewed but never played
  through says when it was last viewed, on a server that says so.
- Tracks now report where they are as they play, every 10 s as films
  and episodes do, so their plays are counted; a track still has no
  resume point and never shows in Continue watching.
- Settings › About › Open source licenses: the 471 components the client
  is built from (the Spoolframe SDK and what it pulls in, this app's own
  crates and the rest it links, the Catppuccin palette, the Lucide icons
  and the four fonts, and the Flatpak's libfido2 and libcbor),
  searched and filtered by license, each with its license text to read
  and copy. The list is made from Cargo.lock and the Flatpak manifest by
  `scripts/gen-licenses.py`; CI fails when it is out of date.

## 0.6.1 — 2026-10-10

Released with the server's 0.6.1 (wire revision 14).

- Admins make, list and revoke invites in Preferences › Invites (PX-44),
  and copy a link that only finds the server.
- A Flatpak of the client, built as a bundle (`flatpak/`).
- Mocha is the default theme until another is picked.
- In a Flatpak, the app's confinement lets oo7 keep its keyring file, and
  a system gst-plugin-gtk4 (GNOME's runtime has one) is used in place of
  the linked-in one.

- The project skeleton, made with `foundry-make` from `__templates/rust`.
