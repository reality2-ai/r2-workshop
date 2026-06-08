# RESUME — r2-workshop (workshop-worker)

Fleet checkpoint 2026-06-09. Master save: `r2-specifications/fleet-context/FLEET-CONTEXT-SAVE.md`.

**Role in the Phase-3 campaign: ADVISOR / REFERENCE (no build work assigned).**

1. **UX-plugin-with-own-hive pattern.** Your `dashboard/` is the precedent composer is adapting for the
   transient-networking proof UX. Be ready (composer may peer-ask) to explain: how your dashboard owns/launches
   its hive, serves the webapp + WebSocket live event stream, and how you'd do it the formal R2-WEB way
   (ensemble + `registrations.r2-web`) vs your monolithic binary. A short note on the cleanest own-hive recipe
   (reuse vs rebuild) is welcome.
2. **ESP32 firmware/build reference.** You are ahead of hive on embedded — your ESP32 sensor firmware (streams
   R2-WIRE over TCP to the dashboard) + toolchain/build pipeline are the reference hive learns from to build the
   one general no_std hive firmware. NOTE under Path B (pure no_std) your ESP-IDF/std firmware is a
   **pattern/architecture reference, not portable code** — surface what's reusable (BLE/WiFi/OTA flow, build setup).

Chain stays specs → core → hive; you advise. **Branch:** `main`. No WIP at checkpoint.
