# ADR-0103: What plays, in the sound panel

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0100 (the sound panel), ADR-0102 (media keys for the
  player), ADR-0094 (Music)
- Part of Phase 10 (Alpha: basic apps).

## Context

ADR-0102 sent the media keys to the player that asked for them, but the
desktop could not show what plays or offer its controls: to pause a song
under another app, the user had to know the keys or find Music's window.

**What users expect, on every desktop:**
- **macOS:** Now Playing in Control Center and the menu bar: the title,
  the app, play/pause, previous, next.
- **Windows:** a media flyout above the volume (and on the lock screen):
  the title, the app, the same three buttons.
- **Linux (GNOME, KDE):** the playing app's controls in the top bar's
  menu or the notification list (MPRIS).
- **Everywhere:** next to the volume, the player that would get the media
  keys, its title, and the three buttons.

## Decision

**The player says what it plays** (`op::NOW_PLAYING`, window op 13):
- `[window][state][title]`: stopped, playing or paused
  (`proto::playing`), and a title of one line, at most 80 bytes (empty
  only when stopped).
- Only a window that asked for the media keys may say it: what the desktop
  shows is always the player its buttons and keys reach.
- The desktop keeps it with the player; it goes when the window does.

**The desktop shows it** in the sound panel (the speaker in the menu bar,
ADR-0100):
- Under the volume, for the player with the media keys once it has
  given a title: the title, its app's name as Core verified it, and Prev,
  Play or Pause, Next.
- The buttons press the media keys for that player, as the keyboard's
  would; the desktop logs them (`desktop: Play/Pause for Music, from the
  sound panel`).

**Music** says it whenever it changes: the song playing (its name without
the folder), or the one selected when stopped.

## Consequences

- What plays can be seen and controlled from any app, beside the volume.
- **Not yet:**
  - artwork, artist, album and a position;
  - showing it outside the panel (the menu bar, the lock screen);
  - several players at once (only the one with the keys).

## Alternatives considered

- **Any app may say what it plays:** an app could put any text next to
  the system's controls without being the player they reach.
- **The desktop guessing from the audio service:** it knows sessions,
  not apps, titles or states.
- **Its own panel or menu-bar item:** next to the volume is where other
  systems put it, and it keeps the menu bar short.

## Checklist (master spec §48)

- **Purpose:** see and control what plays from the desktop.
- **Architecture:** `NOW_PLAYING`, `proto::playing`, `now_playing` (the
  check) in `oceans-window`; `Manager::set_now_playing`, `now_playing`,
  `media_key`; the panel's section and `Hit::Media` in the desktop;
  `oceans_display_proto::now_playing`, `Ui::now_playing`; Music's
  `report`.
- **API:** the window op `NOW_PLAYING` (13).
- **Dependencies:** none.
- **Security:** only the player with the media keys may say, about its own
  window; the app's name shown is Core's, not the app's.
- **Testing:**
  - unit (`oceans-window`): only a player that asked may say; states and
    titles are checked; stopped with no title hides it; the player shown
    follows the keys, and windows closing; the buttons press its keys;
  - smoke: with Music playing, the speaker opens the panel with Music's
    song and a Play button; Next reaches Music, as the desktop logs (Play
    would play the song again, which the sound check counts).
- **Failure behaviour:**
  - **A bad report:** refused (`BadRequest`, `NotAllowed`); the panel shows
    what it showed.
  - **The player gone:** its section goes.
