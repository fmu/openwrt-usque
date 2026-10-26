/*
 * Host-side smoke test for the luci-app-usque LuCI view (v2, multi-connection).
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
		addEventListener() {},
		removeEventListener() {},
		setAttribute() {},
		getAttribute() { return null; },
		querySelector() { return null; },
		querySelectorAll() { return []; },
		classList: {
			add(c) {
				const set = new Set(String(this._node.className || '').split(/\s+/).filter(Boolean));
				set.add(c);
				this._node.className = Array.from(set).join(' ');
			},
			remove(c) {
				const set = new Set(String(this._node.className || '').split(/\s+/).filter(Boolean));
				set.delete(c);
				this._node.className = Array.from(set).join(' ');
			},
			contains(c) { return String(this._node.className || '').split(/\s+/).indexOf(c) !== -1; }
		}
	};

	node.classList._node = node;
	Object.defineProperty(node, 'textContent', {
		get() { return this._textContent; },
		set(v) { this._textContent = (v == null) ? '' : String(v); }
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
		node.childNodes.push(children);
		if (node.tagName === 'SELECT' && !node.value &&
		    children.attributes && children.attributes.value != null)
			node.value = String(children.attributes.value);
		return;
	}
	node.textContent = String(node.textContent || '') + String(children);
}

function textOfList(children) {
	let out = '';
	(children || []).forEach(function(c) { out += textOf(c); });
	return out;
}

function textOf(node) {
	if (!isEl(node))
		return node == null ? '' : String(node);
	let out = node.textContent || '';
	node.childNodes.forEach(function(c) { out += textOf(c); });
	return out;
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
		return String(v);
	});
};

/* ---------- LuCI stubs ---------- */
const notifications = [];
const modals = [];
const pollFns = [];
const rpcCalls = [];
const initActions = [];

let statusFixture = null;
let getConfigFixture = { ok: true, exists: true, valid: true };
let logsFixture = { ok: true, count: 2, lines: 'line one\nline two' };
let testFixture = null;
let registerFixture = { ok: true, started: true };
let registerStatusFixture = null;
let importFixture = { ok: true, imported: true };
let deleteFixture = { ok: true, deleted: true };
let permFixture = { ok: true, mode: '-rw-------' };
let uciSectionsFixture = [ { '.name': 'main', '.type': 'usque', enabled: '1', mode: 'cloudflare', interface: 'warp0' } ];

global.L = {
	env: { sessionid: 'x', pollinterval: 5 },
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

function fixtureFor(method) {
	switch (method) {
		case 'status': return statusFixture;
		case 'getConfig': return getConfigFixture;
		case 'logs': return logsFixture;
		case 'test': return testFixture;
		case 'register': return registerFixture;
		case 'registerStatus': return registerStatusFixture;
		case 'importConfig': return importFixture;
		case 'deleteConfig': return deleteFixture;
		case 'fixConfigPermissions': return permFixture;
		case 'setInitAction': return { ok: true };
		case 'apply': return {};
		default: return { ok: true };
	}
}

const rpc = {
	declare: function(opts) {
		return function() {
			const args = Array.prototype.slice.call(arguments);

			rpcCalls.push({ object: opts.object, method: opts.method, args: args });
			if (opts.method === 'setInitAction')
				initActions.push(args[0]);

			return Promise.resolve(fixtureFor(opts.method));
		};
	}
};

const poll = {
	add: function(fn, interval) { pollFns.push(fn); return true; },
	remove: function(fn) { const i = pollFns.indexOf(fn); if (i !== -1) pollFns.splice(i, 1); return true; }
};

const ui = {
	createHandlerFn: function(ctx, name, ...args) {
		const fn = ctx[name];

		if (typeof fn !== 'function')
			throw new Error('createHandlerFn: missing handler ' + name);

		return function(ev) { return fn.call(ctx, ...args, ev || { currentTarget: El('button') }); };
	},
	addNotification: function(title, content, cls) {
		notifications.push({ title: String(title), content: content, cls: cls });
	},
	showModal: function(title, children) { modals.push({ title: String(title), children: children }); },
	hideModal: function() {},
	changes: { setIndicator: function() {} }
};

const uciStore = {
	'usque.main.interface': 'warp0',
	'usque.main.mode': 'cloudflare',
	'usque.settings.lan_ipv6': '0'
};

const uciWrites = [];

const uci = {
	get: function(c, s, o) { const k = c + '.' + s + '.' + o; return Object.prototype.hasOwnProperty.call(uciStore, k) ? uciStore[k] : null; },
	set: function(c, s, o, v) { uciWrites.push([ 'set', c, s, o, v ]); uciStore[c + '.' + s + '.' + o] = v; },
	unset: function(c, s, o) { uciWrites.push([ 'unset', c, s, o ]); delete uciStore[c + '.' + s + '.' + o]; },
	load: function() { return Promise.resolve(); },
	sections: function(config, type) { return uciSectionsFixture.slice(); }
};

const dom = {
	content: function(node, children) {
		node.childNodes = [];
		node.textContent = '';
		append(node, children);
		return node;
	},
	append: append,
	findClassInstance: function() { return null; }
};

/* ---------- form stub ---------- */
function Option(cls, name, title) {
	this.cls = cls;
	this.option = name;
	this.title = title;
	this.values = [];
}
Option.prototype.value = function(v, l) { this.values.push([ v, l ]); return this; };
Option.prototype.depends = function() { return this; };

function Section(cls, args) {
	this.cls = cls;
	this.args = args;
	this.options = [];
}
Section.prototype.option = function(cls, name, title) {
	const o = new Option(cls, name, title);
	this.options.push(o);
	return o;
};

const form = {
	NamedSection: 'NamedSection',
	GridSection: 'GridSection',
	Flag: 'Flag',
	Value: 'Value',
	ListValue: 'ListValue',
	MultiValue: 'MultiValue',
	DummyValue: 'DummyValue',
	Map: function(config, title, description) {
		this.config = config;
		this.title = title;
		this.description = description;
		this.mapEl = El('div', { 'class': 'cbi-map' });
		this.sections = [];
		this.section = function(cls, ...args) {
			const s = new Section(cls, args);
			this.sections.push(s);
			return s;
		};
		this.render = function() { return Promise.resolve(this.mapEl); };
		this.save = function() { return Promise.resolve(); };
		this.data = {
			add: function(config, type, name) {
				const sid = name || 'cfg' + (1000 + uciSectionsFixture.length);
				uciSectionsFixture.push({ '.name': sid, '.type': type });
				return sid;
			},
			sections: function() { return uciSectionsFixture.slice(); },
			get: function(c, sid, opt) { return uci.get(c, sid, opt); },
			set: function() {},
			remove: function(c, sid) {
				uciSectionsFixture = uciSectionsFixture.filter(function(s) { return s['.name'] !== sid; });
			}
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

/* ---------- module loading ---------- */
const src = fs.readFileSync(viewPath, 'utf8');

function parseLuCIRequires(source) {
	const requirematch = /^require[ \t]+(\S+)(?:[ \t]+as[ \t]+([a-zA-Z_]\S*))?$/;
	const strictmatch = /^use[ \t]+strict$/;
	const depends = [];
	let off = -1, prev = -1, quote = -1, comment = -1, esc = false;

	for (let i = 0; i < source.length; i++) {
		const chr = source.charCodeAt(i);

		if (esc) {
			esc = false;
		}
		else if (comment != -1) {
			if ((comment == 47 && chr == 10) || (comment == 42 && prev == 42 && chr == 47))
				comment = -1;
		}
		else if ((chr == 42 || chr == 47) && prev == 47) {
			comment = chr;
		}
		else if (chr == 92) {
			esc = true;
		}
		else if (chr == quote) {
			const str = source.substring(off, i);
			const m = requirematch.exec(str);

			if (m)
				depends.push(m[1]);
			else if (!strictmatch.exec(str))
				break;

			off = -1;
			quote = -1;
		}
		else if (quote == -1 && (chr == 34 || chr == 39)) {
			off = i + 1;
			quote = chr;
		}

		prev = chr;
	}

	return depends;
}

const EXPECTED_REQUIRES = [ 'view', 'form', 'rpc', 'poll', 'ui', 'uci', 'dom' ];
const REQUIRES = parseLuCIRequires(src);

const factory = new Function(
	'window', 'document', 'L', 'view', 'form', 'rpc', 'poll', 'ui', 'uci', 'dom',
	src + '\n'
);

/* ---------- fixtures ---------- */
function conn(over) {
	return Object.assign({
		section: 'main',
		name: 'main',
		iface: 'warp0',
		mode: 'cloudflare',
		enabled: true,
		config_file: '/etc/usque/warp0.json',
		config_exists: true,
		config_valid: true,
		config_secure: true,
		config_mode: '-rw-------',
		config_size: 710,
		mtu: '1400',
		endpoint_port: '443',
		always_reconnect: true,
		auto_register: true,
		device_name: '',
		log_level: 'info',
		sni: '',
		default_route: false,
		metric: 0,
		ipv6: true,
		ipv6_route: 'auto',
		running: true,
		pid: 4242,
		uptime: 3600,
		iface_exists: true,
		iface_up: true,
		iface_ipv4: '172.16.0.2',
		iface_ipv6: '2606:4700:110::2',
		route4_installed: false,
		route6_installed: false,
		device_id: 'device-1',
		warp_ipv4: '172.16.0.2',
		warp_ipv6: '2606:4700:110::2',
		endpoint_v4: '162.159.198.2',
		endpoint_v6: '2606:4700:103::2'
	}, over || {});
}

function makeStatus(connections, globals, service) {
	return {
		ok: true,
		service: Object.assign({
			init_script: true,
			boot_enabled: true,
			enabled_any: true,
			running_any: true,
			connection_count: connections.length
		}, service || {}),
		globals: Object.assign({
			lan_ipv6: false,
			lan_ipv6_prefix: '',
			lan_device: 'br-lan',
			lan_ula: 'fd12:3456:789a::/48',
			lan_ra: 'server',
			lan_ipv6_addrs: 'fd12:3456:789a:1::1 ',
			wan_ipv6_default: false,
			lan_ipv6_active: false
		}, globals || {}),
		connections: connections
	};
}

function assert(cond, msg) {
	if (!cond)
		throw new Error(msg);
}

function lastNotification() {
	return notifications[notifications.length - 1];
}

function rpcMethodsSince(index) {
	return rpcCalls.slice(index).map(function(c) { return c.method; });
}

/* ---------- run ---------- */
(async function run() {
	let failures = 0;

	const t = function(name, fn) {
		return Promise.resolve().then(fn).then(function() {
			console.log('ok: ' + name);
		}).catch(function(err) {
			console.error('FAIL: ' + name + ': ' + err.message);
			if (err.stack)
				console.error(err.stack.split('\n').slice(1, 3).join('\n'));
			failures++;
		});
	};

	let v;

	await t('module header parses as LuCI require list', function() {
		assert(JSON.stringify(REQUIRES) === JSON.stringify(EXPECTED_REQUIRES),
			'requires: ' + JSON.stringify(REQUIRES));
	});

	v = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);

	await t('module returns a view object with the expected handlers', function() {
		assert(typeof v.load === 'function', 'load()');
		assert(typeof v.render === 'function', 'render()');
		assert(typeof v.handleSaveApply === 'function', 'handleSaveApply()');
		assert(typeof v.handleStartSection === 'function', 'handleStartSection()');
		assert(typeof v.handleImport === 'function', 'handleImport()');
		assert(typeof v.handleRemoveConnection === 'function', 'handleRemoveConnection()');
	});

	await t('load() resolves status/logs/uci', async function() {
		statusFixture = makeStatus([ conn() ]);
		const data = await v.load();
		assert(Array.isArray(data) && data.length === 3, 'three results');
		assert(data[0] && data[0].connections, 'status payload');
	});

	let root;

	await t('render() builds the five page sections', async function() {
		root = await v.render([ statusFixture, logsFixture, null ]);
		assert(isEl(root), 'element');
		assert(root.childNodes.length === 5, 'overview + connections + map + account + diagnostics, got ' + root.childNodes.length);
	});

	await t('form is built with global settings + connection grid', function() {
		assert(v.map.sections.length === 2, 'two sections, got ' + v.map.sections.length);
		assert(v.map.sections[0].cls === 'NamedSection', 'settings section');
		assert(v.map.sections[1].cls === 'GridSection', 'grid section');
		const grid = v.map.sections[1];
		assert(grid.addremove === true, 'grid addremove');
		assert(typeof grid.sectiontitle === 'function', 'grid sectiontitle');
		const names = grid.options.map(function(o) { return o.option; });
		[ 'enabled', 'interface', 'mode', 'default_route', 'ipv6_route', 'ipv6', 'metric',
		  'config_file', 'mtu', 'endpoint_port', 'always_reconnect', 'auto_register',
		  'device_name', 'sni', 'authority', 'protocol', 'log_level' ].forEach(function(n) {
			assert(names.indexOf(n) !== -1, 'grid option ' + n);
		});
		const settings = v.map.sections[0];
		const snames = settings.options.map(function(o) { return o.option; });
		assert(snames.indexOf('lan_ipv6') !== -1 && snames.indexOf('lan_ipv6_prefix') !== -1, 'lan ipv6 options');
	});

	await t('custom-mode extras (sni/authority/protocol) are modal options', function() {
		const grid = v.map.sections[1];
		[ 'sni', 'authority', 'protocol' ].forEach(function(name) {
			const o = grid.options.filter(function(x) { return x.option === name; })[0];
			assert(o && o.modalonly === true, name + ' is modalonly');
		});
		const proto = grid.options.filter(function(x) { return x.option === 'protocol'; })[0];
		const values = proto.values.map(function(v) { return v[0]; });
		assert(values.indexOf('') !== -1 && values.indexOf('connect-ip') !== -1,
			'protocol values: ' + JSON.stringify(values));
	});

	await t('custom-only options are cleared when mode is not custom', function() {
		const grid = v.map.sections[1];
		const authority = grid.options.filter(function(x) { return x.option === 'authority'; })[0];
		const protocol = grid.options.filter(function(x) { return x.option === 'protocol'; })[0];
		uciStore['usque.main.mode'] = 'cloudflare';
		uciWrites.length = 0;
		authority.write.call({ map: v.map }, 'main', 'https://example.com');
		protocol.write.call({ map: v.map }, 'main', 'connect-ip');
		assert(uciWrites.length === 2 && uciWrites[0][0] === 'unset' && uciWrites[1][0] === 'unset',
			'cloudflare mode clears custom options: ' + JSON.stringify(uciWrites));

		uciStore['usque.main.mode'] = 'custom';
		uciWrites.length = 0;
		authority.write.call({ map: v.map }, 'main', 'https://example.com');
		protocol.write.call({ map: v.map }, 'main', 'connect-ip');
		assert(uciWrites[0][0] === 'set' && uciWrites[0][4] === 'https://example.com', 'authority written in custom mode');
		assert(uciWrites[1][0] === 'set' && uciWrites[1][4] === 'connect-ip', 'protocol written in custom mode');
	});

	await t('default_route defaults to OFF (opt-in)', function() {
		const grid = v.map.sections[1];
		const dr = grid.options.filter(function(o) { return o.option === 'default_route'; })[0];
		assert(dr.default === '0', 'default_route default: ' + dr.default);
		const v6 = grid.options.filter(function(o) { return o.option === 'ipv6'; })[0];
		assert(v6.default === '1', 'ipv6 default ON');
		const r6 = grid.options.filter(function(o) { return o.option === 'ipv6_route'; })[0];
		assert(r6.default === 'auto', 'ipv6_route default auto');
	});

	await t('live table shows one row per connection', function() {
		assert(v.statusBody.childNodes.length === 1, 'one row');
		const row = textOf(v.statusBody.childNodes[0]);
		assert(/main/.test(row) && /warp0/.test(row), 'row content: ' + row);
		assert(/Running/.test(v.rowRefs.main.state.textContent), 'state badge');
		assert(/172\.16\.0\.2/.test(v.rowRefs.main.v4.textContent), 'v4 address');
	});

	await t('overview explains that no default route is installed', function() {
		assert(/no default route is installed/.test(textOf(root)), 'opt-in hint present');
	});

	await t('poll is registered and refreshes the table', async function() {
		assert(pollFns.length >= 1, 'poll.add called');
		statusFixture = makeStatus([ conn({ running: false }) ]);
		const before = rpcCalls.length;
		await pollFns[0]();
		assert(rpcCalls.length > before, 'status rpc issued');
		assert(/Stopped/.test(v.rowRefs.main.state.textContent), 'stopped badge');
	});

	await t('default route / ipv6 route badges reflect runtime state', function() {
		v.updateStatus(makeStatus([ conn({ default_route: true, route4_installed: true, ipv6_route: 'always', route6_installed: true }) ]));
		assert(/IPv4 default/.test(textOf(v.rowRefs.main.route)), 'v4 default badge');
		assert(/IPv6 default/.test(textOf(v.rowRefs.main.route)), 'v6 default badge');
		assert(/default route through/.test(textOf(root)), 'overview default route info');
	});

	await t('second connection gets its own row', function() {
		v.updateStatus(makeStatus([
			conn(),
			conn({ section: 'cnt2', iface: 'warp9', mode: 'custom', enabled: false, running: false,
				config_file: '/etc/usque/cnt2.json', config_exists: false, config_valid: false,
				config_secure: false, config_mode: '' })
		]));
		assert(v.statusBody.childNodes.length === 2, 'two rows');
		assert(v.rowRefs.cnt2, 'refs for cnt2');
		assert(/Custom/.test(textOf(v.rowRefs.cnt2.mode)), 'custom mode badge');
		assert(/Disabled/.test(v.rowRefs.cnt2.state.textContent), 'disabled badge');
		assert(/no config/.test(textOf(v.rowRefs.cnt2.iface)), 'missing config badge');
	});

	await t('warnings flag enabled connections without configuration', function() {
		v.updateStatus(makeStatus([ conn({ enabled: true, config_exists: false, config_valid: false }) ]));
		assert(/has no configuration file yet/.test(textOf(root)), 'missing config warning');
	});

	/* --- per-connection actions --- */
	await t('start refuses for disabled connection', async function() {
		const before = initActions.length;
		const beforeNotifs = notifications.length;
		await v.handleStartSection('cnt2');
		assert(initActions.length === before, 'no init action');
		assert(notifications.length === beforeNotifs + 1 && lastNotification().cls === 'warning', 'warning');
	});

	await t('start refuses without a config file', async function() {
		v.updateStatus(makeStatus([ conn({ enabled: true, config_exists: false }) ]));
		const before = initActions.length;
		await v.handleStartSection('main');
		assert(initActions.length === before, 'no init action');
		assert(lastNotification().cls === 'warning', 'warning');
	});

	await t('start/stop/restart call setInitAction with the section', async function() {
		statusFixture = makeStatus([ conn() ]);
		v.updateStatus(statusFixture);
		await v.handleStartSection('main');
		await v.handleStopSection('main');
		await v.handleRestartSection('main');
		const calls = rpcCalls.filter(function(c) { return c.method === 'setInitAction'; }).slice(-3);
		assert(calls[0].args[0] === 'start' && calls[0].args[1] === 'main', 'start main');
		assert(calls[1].args[0] === 'stop' && calls[1].args[1] === 'main', 'stop main');
		assert(calls[2].args[0] === 'restart' && calls[2].args[1] === 'main', 'restart main');
	});

	await t('test runs per section and stores the result', async function() {
		testFixture = { ok: true, warp: 'on', ip: '104.28.0.1', colo: 'VIE', loc: 'AT',
			bound: true, timestamp: '2026-01-01T00:00:00Z', method: 'curl --interface warp0', raw: 'warp=on\n' };
		await v.handleTestSection('main');
		assert(v.lastTest.main && v.lastTest.main.warp === 'on', 'lastTest stored');
		assert(lastNotification().cls === 'success', 'success notification');
		assert(/egress test/.test(textOf(v.rowRefs.main.route)), 'row shows egress result');
	});

	await t('test failure is reported visibly', async function() {
		testFixture = { ok: false, error: 'no connectivity' };
		await v.handleTestSection('main');
		assert(lastNotification().cls === 'danger', 'danger notification');
		assert(v.lastTest.main.warp === 'on', 'previous result kept');
	});

	await t('logs open a modal with the filtered tail', async function() {
		logsFixture = { ok: true, count: 1, lines: 'main: test log line' };
		const before = modals.length;
		await v.handleLogsSection('main');
		assert(modals.length === before + 1, 'modal opened');
		assert(/test log line/.test(textOfList(modals[modals.length - 1].children)), 'log content');
	});

	/* --- account panel: cloudflare --- */
	await t('account panel selects the first connection', function() {
		v.updateStatus(makeStatus([ conn() ]));
		v.accountSectionFor = null;
		v.updateAccountSection();
		assert(v.acctSel.childNodes.length >= 1, 'select populated');
		assert(v.acctSel.value === 'main', 'select value: ' + v.acctSel.value);
		assert(/WARP device ID/.test(textOf(v.acctBody)), 'summary shown');
		assert(/device-1/.test(textOf(v.acctBody)), 'device id shown');
	});

	await t('generate without ToS only warns', async function() {
		v.updateStatus(makeStatus([ conn({ config_exists: false, config_valid: false }) ]));
		v.acctSel.value = 'main';
		v.accountSectionFor = null;
		v.updateAccountSection();
		const before = notifications.length;
		await v.handleGenerate();
		assert(notifications.length === before + 1, 'notification');
		assert(lastNotification().cls === 'warning' && /Terms of Service/.test(textOf(lastNotification().content)), 'ToS warning');
	});

	await t('generate with ToS starts registration and polls', async function() {
		v.regTos.checked = true;
		v.regNameInput.value = '  my router  ';
		const before = rpcCalls.length;
		await v.handleGenerate();
		const reg = rpcCalls.slice(before).filter(function(c) { return c.method === 'register'; })[0];
		assert(reg, 'register rpc called');
		assert(reg.args[0] === 'main' && reg.args[1] === 'my router' && reg.args[2] === true, 'register args');
		assert(v.registering === 'main', 'registration in progress');
		assert(v.regBox.style.display === '', 'progress visible');
	});

	await t('registerStatus progress and success', async function() {
		registerStatusFixture = { ok: true, running: true, done: false, timed_out: false, rc: null,
			elapsed: 7, output: 'connecting...', config_exists: false };
		await pollFns[0]();
		assert(/7s/.test(v.regText.textContent), 'elapsed: ' + v.regText.textContent);

		registerStatusFixture = { ok: true, running: false, done: true, timed_out: false, rc: 0,
			elapsed: 9, output: '', config_exists: true };
		statusFixture = makeStatus([ conn({ config_exists: true }) ]);
		await pollFns[0]();
		assert(v.registering === null, 'registration finished');
		assert(v.regBox.style.display === 'none', 'progress hidden');
		assert(lastNotification().cls === 'success', 'success notification');
	});

	await t('failed registration keeps the output visible', async function() {
		v.registering = 'main';
		v.regT0 = Date.now();
		registerStatusFixture = { ok: true, running: false, done: true, timed_out: false, rc: 1,
			elapsed: 4, output: 'error: registration failed', config_exists: false };
		await v.updateRegister('main', registerStatusFixture);
		assert(v.registering === null, 'finished');
		assert(v.regBox.style.display === '', 'progress still visible');
		assert(/exit code 1/.test(v.regText.textContent), 'failure text');
		assert(v.regOutput.textContent === 'error: registration failed', 'output kept');
	});

	/* --- account panel: custom (import) --- */
	await t('custom mode renders the JSON import panel', function() {
		v.updateStatus(makeStatus([ conn({ section: 'cnt2', iface: 'warp9', mode: 'custom',
			config_file: '/etc/usque/cnt2.json', config_exists: false }) ]));
		v.acctSel.value = 'cnt2';
		v.accountSectionFor = null;
		v.updateAccountSection();
		assert(/Custom endpoint/.test(textOf(v.acctBody)), 'custom hint');
		assert(/IP literals/.test(textOf(v.acctBody)), 'IP literal hint');
		assert(/SPKI/.test(textOf(v.acctBody)), 'SPKI pinning hint');
		assert(v.importArea, 'textarea exists');
	});

	await t('import rejects empty / non-object payloads', async function() {
		v.importArea.value = '';
		let before = notifications.length;
		await v.handleImport();
		assert(notifications.length === before + 1 && lastNotification().cls === 'warning', 'empty warning');

		v.importArea.value = '[]';
		before = notifications.length;
		await v.handleImport();
		assert(notifications.length === before + 1 && /JSON object/.test(textOf(lastNotification().content)), 'object warning');
	});

	await t('import calls importConfig for valid input', async function() {
		v.importArea.value = '{"private_key":"x","ipv4":"172.16.0.9"}';
		const before = rpcCalls.length;
		await v.handleImport();
		const imp = rpcCalls.slice(before).filter(function(c) { return c.method === 'importConfig'; })[0];
		assert(imp && imp.args[0] === 'cnt2', 'import rpc');
		assert(lastNotification().cls === 'success', 'success notification');
	});

	/* --- delete config --- */
	await t('delete config shows a confirmation modal and calls the rpc', async function() {
		v.updateStatus(makeStatus([ conn({ section: 'main' }) ]));
		v.acctSel.value = 'main';
		v.accountSectionFor = null;
		v.updateAccountSection();
		await v.handleDeleteConfig();
		const modal = modals[modals.length - 1];
		let btn = null;
		(function walk(node) {
			if (!node || !node.childNodes) return;
			node.childNodes.forEach(function(c) {
				if (isEl(c) && c.tagName === 'BUTTON' && /^Delete$/.test(String(c.textContent)) && typeof c.attributes.click === 'function')
					btn = c;
				walk(c);
			});
		})({ childNodes: modal.children });
		assert(btn, 'delete button found');
		const before = rpcCalls.length;
		await btn.attributes.click();
		assert(rpcMethodsSince(before).indexOf('deleteConfig') !== -1, 'deleteConfig rpc');
	});

	await t('fix permissions calls the rpc', async function() {
		v.updateStatus(makeStatus([ conn({ config_secure: false, config_mode: '-rw-r--r--' }) ]));
		v.acctSel.value = 'main';
		v.accountSectionFor = null;
		v.updateAccountSection();
		const before = rpcCalls.length;
		await v.handleFixPermissions();
		assert(rpcMethodsSince(before).indexOf('fixConfigPermissions') !== -1, 'fix rpc');
	});

	/* --- remove connection --- */
	await t('remove connection stops the instance, drops the section, commits', async function() {
		const before = rpcCalls.length;
		await v.handleRemoveConnection('cnt2');
		const modal = modals[modals.length - 1];
		let btn = null;
		(function walk(node) {
			if (!node || !node.childNodes) return;
			node.childNodes.forEach(function(c) {
				if (isEl(c) && c.tagName === 'BUTTON' && /^Remove$/.test(String(c.textContent)) && typeof c.attributes.click === 'function')
					btn = c;
				walk(c);
			});
		})({ childNodes: modal.children });
		assert(btn, 'remove button found');
		await btn.attributes.click();
		const methods = rpcMethodsSince(before);
		assert(methods.indexOf('setInitAction') !== -1, 'init action called');
		assert(methods.indexOf('apply') !== -1, 'uci apply called');
		assert(!uciSectionsFixture.some(function(s) { return s['.name'] === 'cnt2'; }), 'section removed');
	});

	/* --- save & apply --- */
	await t('save & apply commits, enables and restarts when a config exists', async function() {
		uciSectionsFixture = [ { '.name': 'main', '.type': 'usque', enabled: '1' } ];
		statusFixture = makeStatus([ conn() ]);
		v.updateStatus(statusFixture);
		initActions.length = 0;
		const before = rpcCalls.length;
		await v.handleSaveApply({});
		const methods = rpcMethodsSince(before);
		assert(methods.indexOf('apply') !== -1, 'uci apply');
		assert(initActions.indexOf('enable') !== -1, 'enable');
		assert(initActions.indexOf('restart') !== -1, 'restart');
		assert(lastNotification().cls === 'success', 'success notification');
	});

	await t('save & apply disables and stops when no connection is enabled', async function() {
		uciSectionsFixture = [ { '.name': 'main', '.type': 'usque', enabled: '0' } ];
		initActions.length = 0;
		await v.handleSaveApply({});
		assert(initActions.indexOf('disable') !== -1, 'disable');
		assert(initActions.indexOf('stop') === -1, 'no per-section stop');
		assert(initActions.indexOf('restart') === -1, 'no restart');
	});

	await t('save & apply skips restart when an enabled connection has no config', async function() {
		uciSectionsFixture = [ { '.name': 'main', '.type': 'usque', enabled: '1' } ];
		statusFixture = makeStatus([ conn({ config_exists: false, config_valid: false }) ]);
		v.updateStatus(statusFixture);
		initActions.length = 0;
		await v.handleSaveApply({});
		assert(initActions.indexOf('enable') !== -1, 'enable');
		assert(initActions.indexOf('restart') === -1, 'no restart');
		assert(lastNotification().cls === 'warning', 'warning notification');
	});

	/* --- degraded backend --- */
	await t('renders without a working backend', async function() {
		const origDeclare = rpc.declare;
		rpc.declare = function() { return function() { return Promise.reject(new Error('Access denied')); }; };
		const v2 = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);
		rpc.declare = origDeclare;
		const data = await v2.load();
		const r2 = await v2.render(data);
		assert(isEl(r2), 'page renders');
		assert(/No connections configured/.test(textOf(r2)), 'hint shown');
	});

	await t('renders when /etc/config/usque cannot be read', async function() {
		const origMap = form.Map;
		form.Map = function() {
			const m = new origMap();
			m.render = function() { return Promise.reject(new Error('Not found')); };
			return m;
		};
		statusFixture = makeStatus([ conn() ]);
		const v3 = factory(global.window, global.document, global.L, view, form, rpc, poll, ui, uci, dom);
		const data = await v3.load();
		const r3 = await v3.render(data);
		form.Map = origMap;
		assert(isEl(r3), 'fallback page rendered');
		assert(/Cannot load the usque configuration/.test(textOf(r3)), 'error section');
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
