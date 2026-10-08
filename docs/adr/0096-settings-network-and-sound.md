# ADR-0096: Settings: the network and the sound

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0081 (Settings, system apps), ADR-0094 (players: an
  end the audio service badges), ADR-0043 (`INFO6`)
- Part of Phase 10 (Alpha: basic apps).

## Context

Settings showed the system and the apps. The network's configuration and
the sound devices could be seen only in the shell (`ifconfig`,
`play info`). The endpoints that answer those questions also open sockets
and play or record sound. Giving Settings `network` or `sound` would give
it far more than it shows.

## Decision

**Reader ends.** The network stack and the audio service each gain
`READER`, a request on their unbadged endpoint. It returns a client end
with a reserved badge that is never a socket's or a session's:

| Service | Badge | Answers on it |
|---|---|---|
| network (`READER_BADGE`) | `1 << 62` | `INFO`, `INFO6` |
| audio (`READER_BADGE`) | `1 << 61` | `INFO`, `INPUT_INFO` |

- **The network:** opening a socket needs the unbadged end. Every socket
  operation looks its badge up among the open sockets, and the reader's
  badge is never there.
- **The audio service:** `OPEN` takes the unbadged end or a player end
  only.

**The `system-settings` permission** (number 10, "see the network's and
the sound's settings"):
- **Who may have it:** like `manage-apps`, system-only. Only a bundled app
  signed by a boot key may ask for it, and it is not asked.
- **What Core gives:** a reader end of each service, as `use net-info` and
  `use audio-info`. A service that does not answer is logged, and Settings
  shows without it.

**Settings 1.1.0** gains two sections:
- **Network:** status, IPv4 address and prefix, router, DNS server, the
  IPv6 addresses and router, the hardware address.
- **Sound:** the output and the input, as the driver names them.

**Versions of bundled apps.** Core installs a bundled app when it is
missing or older (ADR-0080), so a changed bundled app needs a new version
to reach existing disks:
- Settings is 1.1.0;
- Files is 1.0.1 (its Home fix in ADR-0094 had kept 1.0.0).

**The sound stops gracefully.** The audio service now has `grant = stop`
(ADR-0086). Its stop notification is bound to its endpoint and also
carries its output tick (ADR-0094). Asked to stop, it halts both
streams' DMA and exits. Before, init killed it with its streams set up,
and once a shutdown hung there: init never saw it go.

## Consequences

- The configuration is in Settings, and Settings still cannot connect,
  play or record.
- Other read-only views of a service can use the same pattern: a badge the
  service reserves for questions.
- **Not yet:**
  - changing settings: a static address, the DNS server, the output
    device, the AI model (its own decision: writes need their own
    rights);
  - Wi-Fi (there is no driver).

## Alternatives considered

- **Giving Settings `network` and `sound`:** it could connect anywhere and
  play; and with the audio service's own endpoint, record.
- **Reading through Core:** Core would hold and forward both services'
  answers. A badge on each service's own endpoint needs no new protocol
  in Core and keeps the services' answers their own.

## Checklist (master spec §48)

- **Purpose:** the network's and the sound's configuration in Settings.
- **Architecture:**
  - `READER` in `oceans-net-proto`, `net`, `oceans-audio-proto` and `hda`;
  - `Permission::SystemSettings`, Core's grant;
  - Settings' Network and Sound sections.
- **API:**
  - `op::READER`, `READER_BADGE`, `oceans_net_proto::reader`,
    `oceans_audio_proto::reader`;
  - the `system-settings` permission (also in the bridge's list).
- **Dependencies:** none.
- **Security:**
  - reader ends answer questions only;
  - system-only, checked at install and again when Core starts the app.
- **Testing:**
  - unit: the permission list against the bridge's;
  - smoke: Settings starts with `system-settings`; Network, clicked, shows
    its four rows, which it can only draw from the stack's answer.
- **Failure behaviour:**
  - **A service that is not there:** the section says its settings are not
    available.
  - **No address yet:** Network says DHCP is still asking.
