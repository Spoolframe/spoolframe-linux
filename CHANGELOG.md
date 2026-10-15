# Changelog

## Unreleased

- Movie, show and episode pages show the cast: the first six actors, each
  a round portrait (or their initials) with the part they play; a click
  opens their items. Cast comes with the server's 0.9.0.
- A show's page says when it ran: "2022", "2022–" while it is still
  airing (an episode in the last twelve months), or "2022–2025".
- An episode's page says the day it aired, or its year where the server
  does not know the day.
- The SDK crates are 0.9.0.
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
