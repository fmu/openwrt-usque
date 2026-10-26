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
| `smoke-test.js` | build host (Node.js) | the LuCI view `overview.js` |
| `router-backend-test.sh` | the router (or via ssh) | the rpcd backend `luci.usque` |

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
(`sh`, `jshn`, `uci`, `ip`, `logread` are required). It is safe to run on a
production router: it uses a **test copy** of the backend pointed at
`/etc/usque/test-warp0.json` plus a fake `usque` binary and a fake init script
via the `USQUE_BIN` / `INIT_SCRIPT` environment overrides, so the real
configuration, the real binary and `/etc/init.d/usque` are never touched.

```sh
# from the repository root
ssh root@<router> 'cat > /tmp/luci.usque.src' < openwrt/luci-app-usque/root/usr/libexec/rpcd/luci.usque
ssh root@<router> 'sh -s' < openwrt/luci-app-usque/host-tests/router-backend-test.sh
```

Covers: method listing, ToS enforcement, device-name validation, asynchronous
registration + `registerStatus` polling, rc/watchdog handling, secret redaction,
`/etc/config` permission fix, config deletion, `setInitAction`, egress test
parsing, logs, unknown methods and malformed JSON input. Exit code `0` = pass.

## What `smoke-test.js` covers

* module factory contract (`view.extend`, `load()`, `render()`),
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
