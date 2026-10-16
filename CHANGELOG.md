# Changelog

## Unreleased

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
  them, and a show's page those holding any of its episodes, beside the
  episodes. On a show's page choosing one plays it from your place;
  elsewhere it opens the playlist; right-click does the other. The rows
  are read with the page, so a change shows the next time it opens.
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
