# Changelog

## Unreleased

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
