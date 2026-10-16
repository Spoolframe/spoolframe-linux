# Changelog

## Unreleased

- Movie, show and episode pages show the cast: the first six actors, each
  a round portrait (or their initials) with the part they play; a click
  opens their items. Cast comes with the server's 0.9.0.
- A show's page says when it ran: "2022", "2022–" while it is still
  airing (an episode in the last twelve months), or "2022–2025".
- An episode's page says the day it aired, or its year where the server
  does not know the day.
- The SDK crates are 0.10.0.
- Invites are copied as https links (PX-45): Copy link and Copy invite
  text in Preferences › Invites, and the link that only finds the server,
  give `https://spoolframe.app/i#…`; the QR code keeps the spoolframe://
  one. "Add a server" says it takes either.
- Playlists (PX-40), on a server that has them: a Playlists item in the
  sidebar opens My playlists, Shared with me and Server-wide, with New
  playlist (a name, the kinds it holds and, for an admin, Server-wide).
  A playlist's page lists its entries with their notes (up next, watched,
  unavailable) and has Continue, Play from start, Shuffle, Repeat and Skip
  watched; for now it plays the entry the server picks, not the ones
  after it. Pinned playlists show in the sidebar under Pinned, in your
  order: pin one from its menu or its page, drag to reorder, right-click
  to unpin. A pin that left because the playlist was unshared or deleted
  says so once. Changes made elsewhere show as they happen. A server
  without playlists shows none of this.

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
