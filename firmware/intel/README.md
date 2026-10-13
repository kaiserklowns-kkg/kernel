# Intel Wi-Fi firmware (ADR-0129)

The firmware Intel's Wi-Fi chips load before they work. Oceans' `iwm`
driver loads it into the chip; nothing on the host runs it.

| File | Chips | Source | SHA-256 |
|---|---|---|---|
| `iwlwifi-8000C-36.ucode` | Intel Wireless 8260 (`8086:24f3`, `8086:24f4`) | linux-firmware, `intel/iwlwifi/iwlwifi-8000C-36.ucode` | `479931721f5e168d69d67c297c11738acd75da390adb94f96030a1055f4cf57a` |

Licence: [LICENCE.iwlwifi_firmware](LICENCE.iwlwifi_firmware), Intel's:
redistribution in binary form, unmodified, with the notice; no reverse
engineering. It is not open source; the user accepted it for Wi-Fi
(ADR-0129).
