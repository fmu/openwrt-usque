/*
 * Host-side smoke test for the luci-app-usque LuCI view.
 *
 * It stubs the LuCI runtime (view, form, rpc, poll, ui, uci, dom, E, _) just
 * enough to execute load()/render()/poll/handlers/save-apply and to detect
 * runtime errors (undefined references, wrong API usage, unhandled promise
 * rejections) without a browser or a router.
 *
 * Usage (Node.js >= 14, no npm packages needed):
 *
 *   node openwrt/luci-app-usque/host-tests/smoke-test.js
 *   node openwrt/luci-app-usque/host-tests/smoke-test.js path/to/overview.js
 *
 * The script is host-only test tooling: it is NOT installed by the package.
 */
'use strict';

const fs = require('fs');
const path = require('path');

const viewPath = process.argv[2] || path.join(__dirname, '..',
	'htdocs', 'luci-static', 'resources', 'view', 'usque', 'overview.js');

/* ---------- DOM stub ---------- */
function El(tag, attrs, children) {
	const node = {
		nodeType: 1,
		tagName: String(tag).toUpperCase(),
		attributes: attrs || {},
		childNodes: [],
		style: {},
		_textContent: '',
		className: (attrs && attrs['class']) || '',
		checked: false,
		disabled: false,
		value: '',
		appendChild(c) { this.childNodes.push(c); return c; },
		classList: {
			add(c) {
				const set = new Set(String(node.className || '').split(/\s+/).filter(Boolean));
				set.add(c);
				node.className = Array.from(set).join(' ');
			},
			remove(c) {
				const set = new Set(String(node.className || '').split(/\s+/).filter(Boolean));
				set.delete(c);
				node.className = Array.from(set).join(' ');
			},
			contains(c) { return String(node.className || '').split(/\s+/).indexOf(c) !== -1; }
		}
	};
	Object.defineProperty(node, 'textContent', {
		get: function() { return this._textContent; },
		set: function(v) { this._textContent = (v == null) ? '' : String(v); }
	});
	append(node, children);
	return node;
}

function isEl(x) { return x && typeof x === 'object' && x.nodeType === 1; }

function append(node, children) {
	if (children == null)
		return;
	if (Array.isArray(children)) {
		children.forEach(function(c) { append(node, c); });
		return;
	}
	if (isEl(children)) {
		node.parent = node;
		node.childNodes.push(children);
		return;
	}
	/* text */
	node.textContent = String(node.textContent || '') + String(children);
}

global.E = function(tag, attrs, children) { return El(tag, attrs, children); };

/* ---------- i18n stub ---------- */
global._ = function(s) {
	const str = new String(String(s));
	str.format = function(...args) { return String.prototype.format.apply(String(s), args); };
	return str;
};

String.prototype.format = function(...args) {
	let i = 0;
	return String(this).replace(/%[sdhxju%]/g, function(m) {
		if (m === '%%') return '%';
		const v = args[i++];
		if (m === '%d') return String(parseInt(v, 10) || 0);
		if (m === '%j') return JSON.stringify(v);
		if (m === '%h') return String(v);
		return String(v);
	});
};

/* ---------- LuCI stubs ---------- */
const notifications = [];
const modals = [];
const pollFns = [];
const initActions = [];
const rpcCalls = [];
let statusData = null;
let registerStatusData = null;
let getConfigData = { ok: true, exists: true };
let testData = null;
let logsData = null;

global.L = {
	env: { sessionid: 'x', pollinterval: 5, apply_display: 5 },
	resolveDefault: function(promise, def) {
		return Promise.resolve(promise).catch(function() { return def; });
	},
	url: function() { return '/cgi-bin/luci/admin/ubus'; },
	isObject: function(o) { return o != null && typeof o === 'object'; }
};

global.document = {
	body: { contains: function() { return true; } },
	addEventListener: function() {},
	querySelector: function() { return null; }
};
global.window = { setTimeout: setTimeout, clearTimeout: clearTimeout };

const rpc = {
	declare: function(opts) {
		const method = opts.method;
		return function() {
			rpcCalls.push({ object: opts.object, method: method, args: Array.prototype.slice.call(arguments) });
			if (method === 'setInitAction')
				initActions.push(arguments[0]);
			return Promise.resolve(
				method === 'status' ? statusData :
				method === 'getConfig' ? getConfigData :
				method === 'registerStatus' ? registerStatusData :
				method === 'test' ? testData :
				method === 'logs' ? logsData :
				method === 'register' ? { ok: true, started: true } :
				method === 'setInitAction' ? { ok: true, action: arguments[0] } :
				method === 'deleteConfig' ? { ok: true, deleted: true } :
				method === 'fixConfigPermissions' ? { ok: true, mode: '-rw-------' } :
				{ ok: true }
			);
		};
	}
};

const poll = {
	add: function(fn, interval) { pollFns.push(fn); return true; },
	remove: function(fn) { const i = pollFns.indexOf(fn); if (i !== -1) pollFns.splice(i, 1); return true; }
};

const ui = {
	createHandlerFn: function(ctx, name) {
		const fn = ctx[name];
		if (typeof fn !== 'function')
			throw new Error('createHandlerFn: missing handler ' + name);
		return function(ev) { return fn.call(ctx, ev || { currentTarget: El('button') }); };
	},
	addNotification: function(title, content, cls) { notifications.push({ title: String(title), content: content, cls: cls }); },
	showModal: function(title, children) { modals.push({ title: String(title), children: children }); },
	hideModal: function() {},
	changes: { setIndicator: function() {} }
};

const uciStore = {
	'usque.main.enabled': '1',
	'usque.main.device_name': '',
	'usque.main.config_file': '/etc/usque/warp0.json'
};

const uci = {
	get: function(c, s, o) { const k = c + '.' + s + '.' + o; return Object.prototype.hasOwnProperty.call(uciStore, k) ? uciStore[k] : null; },
	load: function() { return Promise.resolve(); },
	sections: function() { return []; }
};

const dom = {
	content: function(node, children) {
		node.childNodes = [];
		node.textContent = '';
		append(node, children);
		return node;
	},
	append: append
};

function Option(cls, name, title) {
	this.cls = cls; this.option = name; this.title = title;
	this.values = [];
	this.value = function(v, l) { this.values.push([v, l]); return this; };
}
Option.prototype.depends = function() { return this; };

function Section(map, type) {
	this.map = map;
	this.type = type;
	this.options = [];
}
Section.prototype.option = function(cls, name, title) {
	const o = new Option(cls, name, title);
	this.options.push(o);
	return o;
};

const form = {
	NamedSection: function(map, sid, stype, title) { this.sid = sid; this.stype = stype; this.title = title; },
	Flag: 'Flag',
	Value: 'Value',
	ListValue: 'ListValue',
	MultiValue: 'MultiValue',
	Map: function(config, title, description) {
		this.config = config;
		this.title = title;
		this.description = description;
		this.mapEl = El('div', { 'class': 'cbi-map' });
		this.sections = [];
		this.section = function(cls, sid, stype, title) {
			const s = new Section(this, stype);
			s.sid = sid;
			s.title = title;
			this.sections.push(s);
			return s;
		};
		this.render = function() {
			append(this.mapEl, this.sections.map(function(s) { return El('div', { 'class': 'cbi-section' }); }));
			return Promise.resolve(this.mapEl);
		};
	}
};

const viewProto = {
	handleSave: function() { return Promise.resolve(); },
	handleSaveApply: function(ev, mode) { return this.handleSave(ev); },
	super: function(name, args) {
		const fn = viewProto[name];
		if (typeof fn !== 'function')
			throw new ReferenceError(name + ' is not a function in base class');
		return fn.apply(this, args || []);
	}
};

const view = {
	extend: function(props) {
		const obj = Object.create(viewProto);
		Object.keys(props).forEach(function(k) { obj[k] = props[k]; });
		return obj;
	}
};

/* ---------- load module ---------- */
const src = fs.readFileSync(viewPath, 'utf8');
const factory = new Function(
	'window', 'document', 'L', 'view', 'form', 'rpc', 'poll', 'ui', 'uci', 'dom',
	src + '\n'
);

/* ---------- fixtures ---------- */
function baseStatus(over) {
	return Object.assign({
		ok: true,
		installed: true,
		init_script: true,
		enabled: true,
		running: true,
		pid: 1234,
		uptime: 3671,
		iface: 'warp0',
		iface_exists: true,
		iface_up: true,
		iface_ipv4: '172.16.0.2',
		iface_ipv6: '',
		default_route_via_iface: true,
		config_path: '/etc/usque/warp0.json',
		config_exists: true,
		config_valid: true,
		config_secure: true,
		config_mode: '-rw-------',
		device_id: '<REDACTED_WARP_DEVICE_ID>',
		warp_ipv4: '172.16.0.2',
		warp_ipv6: '<WARP_CLIENT_IPV6>',
		endpoint_v4: '162.159.198.2',
		endpoint_v6: '2606:4700:103::2',
		endpoint_h2_v4: '162.159.198.2',
		endpoint_h2_v6: '',
		mtu: '1400'
	}, over || {});
}

const configData = {
	ok: true, exists: true, valid: true, secure: true, path: '/etc/usque/warp0.json',
	mode: '-rw-------', size: 710, id: '104147c1', ipv4: '172.16.0.2',
	ipv6: '2606:4700:110::2', endpoint_v4: '162.159.198.2', endpoint_v6: '',
	endpoint_h2_v4: '162.159.198.2', endpoint_h2_v6: ''
};

const logsSample = { ok: true, count: 2, lines: 'line one\nline two' };

function assert(cond, msg) {
	if (!cond) throw new Error(msg);
}

(async function run() {
	let failures = 0;
	const t = function(name, fn) {
		return Promise.resolve().then(fn).then(function() {
			console.log('ok: ' + name);
		}).catch(function(err) {
			console.error('FAIL: ' + name + ': ' + err.message + '\n' + (err.stack || ''));
			failures++;
		});
	};

	const v = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);

	await t('module returns a view object with load/render', function() {
		assert(typeof v.load === 'function', 'load() present');
		assert(typeof v.render === 'function', 'render() present');
		assert(typeof v.handleSaveApply === 'function', 'handleSaveApply() present');
		assert(typeof v.handleRegister === 'function', 'handleRegister() present');
	});

	/* --- state: configured + running --- */
	statusData = baseStatus();
	logsData = logsSample;
	let data = await v.load();
	await t('load() resolves status/getConfig/logs', function() {
		assert(Array.isArray(data) && data.length === 3, 'three results');
		assert(data[0] && data[0].device_id, 'status payload present');
	});

	let rootEl;
	await t('render() builds a DOM tree', async function() {
		rootEl = await v.render(data);
		assert(isEl(rootEl), 'returned element');
		assert(rootEl.childNodes.length === 4, 'four sections (status, account, map, diagnostics), got ' + rootEl.childNodes.length);
	});

	await t('status section shows Running/up/default route', function() {
		assert(v.elService.textContent === 'Running', 'service badge: ' + v.elService.textContent);
		assert(/warp0/.test(v.elIface.textContent), 'iface text: ' + v.elIface.textContent);
		assert(v.elConfigPerm.className === 'cbi-value-description', 'secure config has no warning badge: ' + v.elConfigPerm.className);
		assert(v.hasCfgBox.style.display === '', 'account summary visible');
		assert(v.noCfgBox.style.display === 'none', 'generate box hidden');
	});

	await t('poll function registered', function() {
		assert(pollFns.length >= 1, 'poll.add called');
	});

	await t('poll tick calls status and updates DOM', async function() {
		statusData = baseStatus({ running: false, enabled: true, pid: 0, uptime: 0 });
		const rpcBefore = rpcCalls.length;
		await pollFns[0]();
		assert(rpcCalls.length > rpcBefore, 'status rpc issued');
		assert(v.elService.textContent === 'Stopped', 'badge: ' + v.elService.textContent);
	});

	/* --- insecure config --- */
	await t('insecure config shows warning + fix button', function() {
		v.updateStatus(baseStatus({ config_secure: false, config_mode: '-rw-r--r--' }));
		assert(/important/.test(v.elConfigPerm.className), 'warning badge: ' + v.elConfigPerm.className);
		assert(v.btnFixPerms.style.display === '', 'fix button visible');
	});

	await t('handleFixPermissions calls rpc and refreshes', async function() {
		statusData = baseStatus();
		v.updateStatus(statusData);
		const before = rpcCalls.length;
		await v.handleFixPermissions();
		assert(rpcCalls.slice(before).some(function(c) { return c.method === 'fixConfigPermissions'; }), 'rpc called');
		assert(v.btnFixPerms.style.display === 'none', 'fix button hidden again');
	});

	/* --- no config yet --- */
	await t('missing config shows the generate box', function() {
		v.updateStatus(baseStatus({
			config_exists: false, config_valid: false, config_secure: false,
			device_id: '', warp_ipv4: '', warp_ipv6: '', config_mode: ''
		}));
		assert(v.noCfgBox.style.display === '', 'generate box visible');
		assert(v.hasCfgBox.style.display === 'none', 'summary hidden');
	});

	await t('handleRegister without ToS only warns', async function() {
		v.regTos.checked = false;
		const notifBefore = notifications.length;
		await v.handleRegister();
		assert(notifications.length === notifBefore + 1, 'warning notification');
		assert(notifications[notifications.length - 1].cls === 'warning', 'warning class');
	});

	await t('handleRegister with ToS starts registration and polls', async function() {
		v.regTos.checked = true;
		v.regNameInput.value = '  my router  ';
		const notifBefore = notifications.length; /* notification only on completion */
		await v.handleRegister();
		assert(rpcCalls.some(function(c) { return c.method === 'register' && c.args[0] === 'my router' && c.args[1] === true; }), 'register rpc with trimmed name + accept_tos=true');
		assert(v.registering === true, 'registration in progress');
		assert(v.regBox.style.display === '', 'progress box visible');
		assert(v.btnGenerate.disabled === true, 'button disabled while running');
	});

	await t('registerStatus progress renders elapsed + output', async function() {
		registerStatusData = { ok: true, running: true, done: false, timed_out: false, rc: null, elapsed: 7, output: 'connecting...', config_exists: false };
		await pollFns[0]();
		assert(/7s/.test(v.regText.textContent), 'elapsed text: ' + v.regText.textContent);
		assert(v.regOutput.textContent === 'connecting...', 'output shown');
	});

	await t('registerStatus done/rc=0 hides progress and notifies', async function() {
		registerStatusData = { ok: true, running: false, done: true, timed_out: false, rc: 0, elapsed: 9, output: '', config_exists: true };
		await pollFns[0]();
		assert(v.registering === false, 'registration finished');
		assert(v.regBox.style.display === 'none', 'progress hidden');
		assert(notifications[notifications.length - 1].cls === 'success', 'success notification');
		assert(v.btnGenerate.disabled === false, 'button re-enabled');
	});

	await t('registerStatus failure keeps the output visible', async function() {
		v.registering = true;
		v.btnGenerate.disabled = true;
		registerStatusData = { ok: true, running: false, done: true, timed_out: false, rc: 1, elapsed: 4, output: 'error: registration failed', config_exists: false };
		await v.updateRegister(registerStatusData);
		assert(v.registering === false, 'registration marked finished');
		assert(v.regBox.style.display === '', 'progress box still visible');
		assert(/exit code 1/.test(v.regText.textContent), 'failure text: ' + v.regText.textContent);
		assert(v.regOutput.textContent === 'error: registration failed', 'output kept');
	});

	/* --- re-register modal / delete modal --- */
	await t('handleReRegister opens a modal', function() {
		const before = modals.length;
		v.handleReRegister();
		assert(modals.length === before + 1, 'modal opened');
	});

	await t('handleDeleteConfig opens a modal', function() {
		const before = modals.length;
		v.handleDeleteConfig();
		assert(modals.length === before + 1, 'modal opened');
	});

	await t('handleDeleteConfig confirm path calls rpc', async function() {
		statusData = baseStatus({ config_exists: false, config_valid: false });
		await v.handleDeleteConfig();
		const modal = modals[modals.length - 1];
		/* find the delete button in the modal tree */
		let deleteBtn = null;
		(function walk(node) {
			if (!node || !node.childNodes) return;
			node.childNodes.forEach(function(c) {
				if (isEl(c) && /Delete/.test(c.textContent) && typeof c.attributes.click === 'function')
					deleteBtn = c;
				walk(c);
			});
		})({ childNodes: modal.children });
		assert(deleteBtn !== null, 'delete button found');
		const before = rpcCalls.length;
		await deleteBtn.attributes.click();
		assert(rpcCalls.slice(before).some(function(c) { return c.method === 'deleteConfig'; }), 'deleteConfig rpc called');
	});

	/* --- service buttons --- */
	await t('handleStart refuses when enabled=0 (init script would no-op)', async function() {
		statusData = baseStatus({ enabled: false, running: false, pid: 0, uptime: 0, init_enabled: false });
		v.updateStatus(statusData);
		const beforeActions = initActions.length;
		const beforeNotifs = notifications.length;
		await v.handleStart();
		assert(initActions.length === beforeActions, 'no init action issued');
		assert(notifications.length === beforeNotifs + 1, 'warning notification');
		assert(notifications[notifications.length - 1].cls === 'warning', 'warning class');
	});

	await t('status note flags enabled-but-not-on-boot', function() {
		v.updateStatus(baseStatus({ enabled: true, init_enabled: false, running: false, pid: 0 }));
		assert(/boot/.test(v.elServiceNote.textContent), 'note: ' + v.elServiceNote.textContent);
	});

	await t('updateStatus keeps the latest status for handlers', function() {
		v.updateStatus(baseStatus({ config_path: '/tmp/other.json' }));
		assert(v.status.config_path === '/tmp/other.json', 'this.status updated');
	});

	await t('handleStart/handleStop/handleRestart call setInitAction', async function() {
		statusData = baseStatus();
		await v.handleStart();
		await v.handleStop();
		await v.handleRestart();
		assert(initActions.indexOf('start') !== -1 && initActions.indexOf('stop') !== -1 && initActions.indexOf('restart') !== -1,
			'actions: ' + initActions.join(','));
	});

	/* --- diagnostics --- */
	await t('handleTest renders a successful trace result', async function() {
		statusData = baseStatus();
		testData = { ok: true, warp: 'on', ip: '104.28.0.1', colo: 'VIE', loc: 'AT', gateway: 'off',
			timestamp: '2026-01-01T00:00:00Z', method: 'curl --interface warp0', bound: true,
			raw: 'warp=on\nip=104.28.0.1\n' };
		await v.handleTest();
		assert(v.lastTest && v.lastTest.warp === 'on', 'lastTest stored');
		assert(v.testResult.childNodes.length > 0, 'result rendered');
	});

	await t('handleTest renders a failed test visibly', async function() {
		testData = { ok: false, error: 'no connectivity' };
		await v.handleTest();
		const json = JSON.stringify(v.testResult, function(k, val) {
			if (k === 'parent') return undefined;
			return val;
		});
		assert(/no connectivity/.test(json), 'error message in result');
		assert(v.lastTest.warp === 'on', 'previous good result kept');
	});

	await t('handleRefreshLogs updates the log viewer', async function() {
		logsData = { ok: true, count: 1, lines: 'fresh log line' };
		await v.handleRefreshLogs();
		assert(v.logPre.textContent === 'fresh log line', 'log text updated');
		assert(/1 entries/.test(v.logCount.textContent), 'count: ' + v.logCount.textContent);
	});

	/* --- save & apply --- */
	await t('handleSaveApply commits uci, applies service state, clears badge', async function() {
		uciStore['usque.main.enabled'] = '1';
		statusData = baseStatus();
		const before = rpcCalls.length;
		await v.handleSaveApply({});
		const calls = rpcCalls.slice(before).map(function(c) { return c.object + '.' + c.method; });
		assert(calls.indexOf('uci.apply') !== -1, 'uci.apply called: ' + calls.join(','));
		assert(calls.indexOf('luci.usque.setInitAction') !== -1, 'setInitAction called');
		assert(initActions.indexOf('enable') !== -1 && initActions.indexOf('restart') !== -1, 'enable+restart: ' + initActions.join(','));
		assert(notifications[notifications.length - 1].cls === 'success', 'success notification');
	});

	await t('handleSaveApply with enabled=0 stops and disables', async function() {
		uciStore['usque.main.enabled'] = '0';
		const before = initActions.length;
		await v.handleSaveApply({});
		const newActions = initActions.slice(before);
		assert(newActions.indexOf('disable') !== -1 && newActions.indexOf('stop') !== -1, 'disable+stop: ' + newActions.join(','));
	});

	await t('handleSaveApply does not restart without a WARP config', async function() {
		uciStore['usque.main.enabled'] = '1';
		getConfigData = { ok: true, exists: false, valid: false };
		const before = initActions.length;
		await v.handleSaveApply({});
		getConfigData = { ok: true, exists: true };
		const newActions = initActions.slice(before);
		assert(newActions.indexOf('enable') !== -1, 'boot symlink still enabled: ' + newActions.join(','));
		assert(newActions.indexOf('restart') === -1, 'no restart without config');
		const notif = notifications[notifications.length - 1];
		assert(notif.cls === 'warning', 'warning notification: ' + notif.cls);
	});

	await t('handleSaveApply surfaces backend errors', async function() {
		uciStore['usque.main.enabled'] = '1';
		const before = notifications.length;
		const origStatus = statusData;
		/* make setInitAction fail once */
		const origRpc = rpc.declare;
		rpc.declare = function(opts) {
			const fn = origRpc(opts);
			if (opts.method === 'setInitAction')
				return function() { return Promise.resolve({ ok: false, error: 'init script missing' }); };
			return fn;
		};
		const v2 = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);
		rpc.declare = origRpc;
		const data2 = await v2.load();
		await v2.render(data2);
		await v2.handleSaveApply({});
		const errs = notifications.slice(before).filter(function(n) { return n.cls === 'danger'; });
		assert(errs.length >= 1, 'danger notification emitted');
		statusData = origStatus;
	});

	/* --- degraded backend --- */
	await t('renders without a working backend (all rpc failing)', async function() {
		const origRpc = rpc.declare;
		rpc.declare = function() { return function() { return Promise.reject(new Error('Access denied')); }; };
		const v3 = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);
		rpc.declare = origRpc;
		const data3 = await v3.load();
		const root3 = await v3.render(data3);
		assert(isEl(root3), 'page still renders');
		assert(v3.elBackend.style.display === '', 'backend error banner visible');
	});

	await t('renders when /etc/config/usque cannot be loaded', async function() {
		const origRpc = rpc.declare;
		rpc.declare = function(opts) {
			if (opts.object === 'uci' && opts.method === 'apply') {
				return function() { return Promise.resolve({}); };
			}
			return origRpc(opts);
		};
		const origMap = form.Map;
		form.Map = function() {
			const m = new origMap();
			m.render = function() { return Promise.reject(new Error('Not found')); };
			return m;
		};
		statusData = baseStatus({ config_exists: false, config_valid: false });
		const v4 = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);
		rpc.declare = origRpc;
		const data4 = await v4.load();
		const root4 = await v4.render(data4);
		form.Map = origMap;
		assert(isEl(root4), 'fallback page rendered');
		const json = JSON.stringify(root4, function(k, val) { return k === 'parent' ? undefined : val; });
		assert(/Cannot load the usque configuration/.test(json), 'error section present');
	});

	if (failures > 0) {
		console.error('\n' + failures + ' test(s) failed');
		process.exit(1);
	}
	console.log('\nall smoke tests passed');
})().catch(function(err) {
	console.error('harness crashed:', err);
	process.exit(2);
});
