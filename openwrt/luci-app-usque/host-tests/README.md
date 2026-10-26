# Host-side tests for luci-app-usque

Extra regression tests for `luci-app-usque` that are **not** installed on the
router. The LuCI view only runs inside a browser against LuCI 25.12's built-in
runtime, which is awkward to reach from CI, so `smoke-test.js` stubs that
runtime and executes the real `overview.js` module body on Node.js; that way
changes to the view are at least checked for runtime errors before they reach a
router.

Nothing in this directory is installed by `luci-app-usque` (see the explicit
install rules in `../Makefile`).

There are two independent test scripts:

| script | runs on | tests |
| --- | --- | --- |
| `smoke-test.js` | build host (Node.js) | the LuCI view `overview.js` (multi-connection UI) |
| `router-backend-test.sh` | the router (or via ssh) | the rpcd backend `luci.usque` (v2, 69 checks) |

## smoke-test.js — LuCI view (host, Node.js)

Requirements: Node.js >= 14, no npm packages, no network access.

```sh
# from the repository root or from this directory
node openwrt/luci-app-usque/host-tests/smoke-test.js

# optionally test another copy of the view
node openwrt/luci-app-usque/host-tests/smoke-test.js /path/to/overview.js
```

Exit code is `0` when all checks pass, `1` when a check fails and `2` when the
harness itself crashes.

## router-backend-test.sh — rpcd backend (on the router)

This one exercises the real shell backend end-to-end on an OpenWrt 25.12 device
(`sh`, `jshn`, `ip`, `logread`, `curl` are required). It is safe to run on a
production router: the backend is driven with a **fake `uci`** (via `PATH`), a
fake `usque` binary (`USQUE_BIN`) and a fake init script (`INIT_SCRIPT`), so the
real `/etc/config/usque`, `/etc/usque/warp0.json` and `/etc/init.d/usque` are
never touched. It creates `/etc/usque/test-warp0.json` and
`/etc/usque/test-custom.json`, removes them at the end and writes one syslog
line to test the per-connection log filter.

```sh
# from the repository root
ssh root@<router> 'cat > /tmp/luci.usque.src' < openwrt/luci-app-usque/root/usr/libexec/rpcd/luci.usque
ssh root@<router> 'sh -s' < openwrt/luci-app-usque/host-tests/router-backend-test.sh
```

Covers (69 checks): multi-section `status` (per-connection UCI/config/interface/
route state + globals), `getConfig`, ToS/device-name validation, asynchronous
per-section registration + `registerStatus` polling, secret redaction, custom
mode JSON import validation and atomic chmod-600 write, per-section
`setInitAction` (including the section argument), egress test parsing, per-
connection log filtering, permission fix, config deletion (including the
`/etc/usque` path guard), LAN IPv6 globals, unknown methods/sections and
malformed JSON input. Exit code `0` = pass.

## What `smoke-test.js` covers

* module factory contract and LuCI `require` header parsing, (`view.extend`, `load()`, `render()`),
* `render()` builds the status / account / configuration / diagnostics sections,
* `poll` registration and the status refresh path (running, stopped, missing
  config, insecure config permissions),
* WARP registration UX: ToS validation, async start, `registerStatus` polling,
  progress/elapsed display, success and failure paths, redacted output,
* re-register and delete confirm modals (including the delete RPC),
* service control buttons (`setInitAction` start/stop/restart),
* egress test rendering (success and failure) and log viewer refresh,
* `Save & Apply`: `uci apply` (commit) followed by enable/disable + restart,
* degraded backends: every RPC failing, and `/etc/config/usque` missing.

## What neither script covers

* Real LuCI rendering, CSS or translations,
* actual `usque` behaviour and routing (that is the on-device test suite's job).

## When the harness needs updating

The stubs mirror the LuCI 26.288 (OpenWrt 25.12) client-side API used by the
view. If the view starts using a new LuCI API, extend the stub objects at the
top of `smoke-test.js` accordingly (`form`, `rpc`, `ui`, `uci`, `poll`, `dom`,
`E`, `_`, `L`).
