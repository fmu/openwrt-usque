'use strict';
'require view';
'require form';
'require rpc';
'require poll';
'require ui';
'require uci';
'require dom';

/*
 * LuCI view for luci-app-usque (v2): Services -> usque (Cloudflare WARP)
 *
 * Supports multiple independent MASQUE/Connect-IP connections (one UCI
 * "config usque '<sid>'" section each), an opt-in default route per connection,
 * IPv6 default-route handling and optional LAN IPv6 (NAT66).
 *
 * The heavy lifting lives in the rpcd backend /usr/libexec/rpcd/luci.usque
 * (ubus object luci.usque); this view only renders state and triggers actions.
 * Secrets (private_key/access_token) are never part of the backend replies and
 * therefore never reach this page.
 */

var DOCS_URL = 'https://fmu.github.io/openwrt-usque/';
var TOS_URL = 'https://www.cloudflare.com/application/terms/';
var POLL_INTERVAL = 5;

var CONFIG_EXAMPLE = '{\n' +
	'  "private_key": "<base64 private key>",\n' +
	'  "endpoint_v4": "192.0.2.1",\n' +
	'  "endpoint_v6": "",\n' +
	'  "endpoint_pub_key": "-----BEGIN PUBLIC KEY-----\\n...\\n-----END PUBLIC KEY-----",\n' +
	'  "ipv4": "172.16.0.2",\n' +
	'  "ipv6": ""\n' +
	'}';

/* rpcd methods (see /usr/libexec/rpcd/luci.usque) */
var callStatus = rpc.declare({ object: 'luci.usque', method: 'status', expect: {} });
var callGetConfig = rpc.declare({ object: 'luci.usque', method: 'getConfig', params: [ 'section' ], expect: {} });
var callRegister = rpc.declare({ object: 'luci.usque', method: 'register', params: [ 'section', 'name', 'accept_tos' ], expect: {} });
var callRegisterStatus = rpc.declare({ object: 'luci.usque', method: 'registerStatus', params: [ 'section' ], expect: {} });
var callImportConfig = rpc.declare({ object: 'luci.usque', method: 'importConfig', params: [ 'section', 'json' ], expect: {} });
var callDeleteConfig = rpc.declare({ object: 'luci.usque', method: 'deleteConfig', params: [ 'section' ], expect: {} });
var callSetInitAction = rpc.declare({ object: 'luci.usque', method: 'setInitAction', params: [ 'action', 'section' ], expect: {} });
var callTest = rpc.declare({ object: 'luci.usque', method: 'test', params: [ 'section' ], expect: {} });
var callLogs = rpc.declare({ object: 'luci.usque', method: 'logs', params: [ 'section' ], expect: {} });
var callFixPermissions = rpc.declare({ object: 'luci.usque', method: 'fixConfigPermissions', params: [ 'section' ], expect: {} });

/* Map.save() only stages changes in the uci session delta; commit them so the
 * init script reads the new /etc/config/usque. "uci apply" is covered by the
 * stock LuCI ACLs (plain "uci commit" is not). */
var callUciApply = rpc.declare({ object: 'uci', method: 'apply', params: [ 'timeout', 'rollback' ], expect: {} });

/* -------------------------------------------------------------------------
 * small helpers
 * ---------------------------------------------------------------------- */

function dash(value) {
	return (value != null && value !== '') ? String(value) : '—';
}

function parseBool(value) {
	return value === true ||
		[ '1', 'on', 'true', 'yes', 'enabled' ].indexOf(String(value).toLowerCase()) !== -1;
}

function fmtDuration(seconds) {
	var sec = parseInt(seconds, 10) || 0;
	var d, h, m, s;

	if (sec <= 0)
		return '—';

	d = Math.floor(sec / 86400);
	h = Math.floor((sec % 86400) / 3600);
	m = Math.floor((sec % 3600) / 60);
	s = sec % 60;

	if (d > 0)
		return '%dd %dh'.format(d, h);
	if (h > 0)
		return '%dh %dm'.format(h, m);
	if (m > 0)
		return '%dm %ds'.format(m, s);
	return '%ds'.format(s);
}

function errText(res) {
	if (!res)
		return _('No reply from the rpcd backend (is luci-app-usque installed and rpcd running?)');
	if (res.error)
		return String(res.error);
	if (res.output)
		return String(res.output);
	if (res.rc != null)
		return _('Command failed with exit code %s').format(res.rc);
	return _('Unknown error');
}

function badge(text, cls) {
	return E('span', { 'class': 'label' + (cls ? ' ' + cls : '') }, text);
}

function setBadge(node, text, cls) {
	node.className = 'label' + (cls ? ' ' + cls : '');
	node.textContent = text;
}

function code(value) {
	return E('code', {}, dash(value));
}

function valueRow(title, content, description) {
	var field = [ content ];
	if (description)
		field.push(E('div', { 'class': 'cbi-value-description' }, description));
	return E('div', { 'class': 'cbi-value' }, [
		E('label', { 'class': 'cbi-value-title' }, title),
		E('div', { 'class': 'cbi-value-field' }, field)
	]);
}

function listCount(list) {
	return (list && list.length) ? list.length : 0;
}

/* -------------------------------------------------------------------------
 * view
 * ---------------------------------------------------------------------- */

return view.extend({
	load: function() {
		return Promise.all([
			L.resolveDefault(callStatus(), null),
			L.resolveDefault(callLogs(), null),
			L.resolveDefault(uci.load('usque'), null)
		]);
	},

	render: function(data) {
		var self = this;

		this.status = (data && data[0]) || {};
		this.logs = (data && data[1]) || {};
		this.lastTest = {};
		this.registering = null;   /* section id while a registration runs */
		this.regSection = null;
		this.rowIds = null;
		this.rowRefs = {};
		this.acctIds = null;
		this.pollActive = true;
		this.pollFn = null;

		var overview = this.renderOverview();
		var conns = this.renderConnections();
		var account = this.renderAccount();
		var diag = this.renderDiagnostics();

		var map = new form.Map('usque', '', _('All connections are independent MASQUE/Connect-IP tunnels. Routing is opt-in: nothing in this page changes the default route unless you enable it below.'));
		this.map = map;
		this.buildForm(map);

		return map.render().then(function(mapNode) {
			self.rootEl = E('div', {}, [ overview, conns, mapNode, account, diag ]);
			self.updateStatus(self.status);
			self.updateAccountSection();
			self.pollFn = function() { return self.refresh(); };
			poll.add(self.pollFn, POLL_INTERVAL);
			return self.rootEl;
		}).catch(function(err) {
			self.rootEl = E('div', {}, [
				overview,
				E('div', { 'class': 'cbi-section' }, [
					E('div', { 'class': 'alert-message danger' }, [
						E('h4', {}, _('Cannot load the usque configuration')),
						E('p', {}, _('Reading /etc/config/usque failed: %s').format(err.message || err)),
						E('p', {}, _('Make sure the usque package (version with multi-connection support) is installed, then reload this page.'))
					])
				]),
				conns, account, diag
			]);
			self.updateStatus(self.status);
			self.updateAccountSection();
			return self.rootEl;
		});
	},

	/* ---------------------------------------------------------------------
	 * overview (live)
	 * ------------------------------------------------------------------ */

	renderOverview: function() {
		this.elBackend = E('div', { 'class': 'alert-message danger', 'style': 'display:none' });
		this.elService = badge(_('unknown'));
		this.elServiceNote = E('span', { 'class': 'cbi-value-description' });
		this.elConnSummary = E('span');
		this.elLanV6 = E('span');
		this.elRouteState = E('span');
		this.elWarnings = E('div', {});

		return E('div', { 'class': 'cbi-section' }, [
			E('h2', {}, _('usque (Cloudflare WARP)')),
			E('div', { 'class': 'cbi-map-descr' },
				_('MASQUE/Connect-IP tunnels. Connections and their routing behaviour are configured below; this page never takes over the default route unless you explicitly enable it.')),
			this.elBackend,
			valueRow(_('Service'), [ this.elService, ' ', this.elServiceNote ]),
			valueRow(_('Connections'), [ this.elConnSummary, ' ', this.elRouteState ]),
			valueRow(_('LAN IPv6'), [ this.elLanV6 ]),
			this.elWarnings
		]);
	},

	updateOverview: function() {
		var st = this.status || {};
		var conns = st.connections || [];
		var service = st.service || {};
		var globals = st.globals || {};

		if (st.ok !== true) {
			this.elBackend.style.display = '';
			this.elBackend.textContent = _('Backend error: %s').format(errText(st));
		}
		else {
			this.elBackend.style.display = 'none';
		}
		var enabled = 0, running = 0, defaultRouted = [], missingCfg = [], lanV6Conns = [];

		conns.forEach(function(c) {
			if (c.enabled) enabled++;
			if (c.running) running++;
			if (c.route4_installed || c.route6_installed) defaultRouted.push(c.iface || c.section);
			if (c.enabled && !c.config_exists) missingCfg.push(c.section);
			if (c.enabled && (c.ipv6_route === 'auto' || c.ipv6_route === 'always'))
				lanV6Conns.push(c.section);
		});

		if (!service.init_script)
			setBadge(this.elService, _('not installed'), 'important');
		else if (running > 0)
			setBadge(this.elService, _('Running'), 'success');
		else if (enabled > 0)
			setBadge(this.elService, _('Stopped'), 'warning');
		else
			setBadge(this.elService, _('Disabled'), '');

		this.elServiceNote.textContent = service.boot_enabled
			? _('enabled on boot')
			: (service.init_script ? _('not enabled on boot') : '');

		this.elConnSummary.textContent = _('%d connection(s), %d running').format(listCount(conns), running);

		if (defaultRouted.length)
			this.elRouteState.textContent = _('default route through: %s').format(defaultRouted.join(', '));
		else
			this.elRouteState.textContent = _('no default route is installed (traffic keeps using the WAN)');

		if (globals.lan_ipv6) {
			if (globals.lan_ipv6_active)
				this.elLanV6.textContent = _('enabled — ULA %s, RA %s, LAN addresses: %s').format(
					dash(globals.lan_ula), dash(globals.lan_ra), dash(globals.lan_ipv6_addrs).trim());
			else
				this.elLanV6.textContent = _('enabled but not active yet (missing ULA, RA server or LAN address)');
		}
		else {
			this.elLanV6.textContent = _('disabled');
		}

		var warnings = [];
		if (!service.init_script)
			warnings.push(E('div', { 'class': 'alert-message danger' }, _('The /etc/init.d/usque init script is missing; reinstall the usque package.')));
		if (!listCount(conns))
			warnings.push(E('div', { 'class': 'alert-message notice' }, _('No connections are configured yet. Add one below; the shipped default section is "warp0" with interface warp0 (UCI: config usque \'warp0\').')));
		missingCfg.forEach(function(sid) {
			warnings.push(E('div', { 'class': 'alert-message warning' }, _('Connection "%s" is enabled but has no configuration file yet. Register a WARP device or import a configuration in the "Account / configuration" section below.').format(sid)));
		});
		if (globals.lan_ipv6 && !globals.lan_ipv6_active)
			warnings.push(E('div', { 'class': 'alert-message warning' }, _('LAN IPv6 is enabled but no LAN IPv6 is active. Check network.globals.ula_prefix, network.lan and dhcp.lan.ra; the init script only sets these up when the option is enabled and a connection provides an IPv6 route.')));
		if (globals.lan_ipv6 && !lanV6Conns.length)
			warnings.push(E('div', { 'class': 'alert-message warning' }, _('LAN IPv6 needs a connection with IPv6 enabled and ipv6_route=auto or always; none is enabled.')));
		if (defaultRouted.length)
			warnings.push(E('div', { 'class': 'alert-message notice' }, _('While a tunnel default route is installed, router and LAN traffic use %s. Disable "Install default route" for a connection to keep the WAN as the default path.').format(defaultRouted.join(', '))));

		dom.content(this.elWarnings, warnings);
	},

	/* ---------------------------------------------------------------------
	 * live connection table
	 * ------------------------------------------------------------------ */

	renderConnections: function() {
		this.statusBody = E('tbody', {});
		this.elNoConns = E('div', { 'class': 'cbi-value-description', 'style': 'display:none' },
			_('No connections configured.'));

		var header = E('tr', { 'class': 'tr cbi-section-table-titles named' }, [
			E('th', { 'class': 'th' }, _('Connection')),
			E('th', { 'class': 'th' }, _('Mode')),
			E('th', { 'class': 'th' }, _('State')),
			E('th', { 'class': 'th' }, _('IPv4')),
			E('th', { 'class': 'th' }, _('IPv6')),
			E('th', { 'class': 'th' }, _('Routing')),
			E('th', { 'class': 'th' }, _('Actions'))
		]);

		return E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('Connections')),
			E('table', { 'class': 'table' }, [ E('thead', {}, header), this.statusBody ]),
			this.elNoConns
		]);
	},

	buildRow: function(sid) {
		var ref = {
			iface: E('td', { 'class': 'td' }),
			mode: E('td', { 'class': 'td' }),
			state: E('td', { 'class': 'td' }),
			v4: E('td', { 'class': 'td' }),
			v6: E('td', { 'class': 'td' }),
			route: E('td', { 'class': 'td' }),
			actions: E('td', { 'class': 'td nowrap' })
		};

		ref.btnStart = E('button', {
			'class': 'cbi-button cbi-button-positive',
			'style': 'margin:1px',
			'click': ui.createHandlerFn(this, 'handleStartSection', sid)
		}, _('Start'));

		ref.btnStop = E('button', {
			'class': 'cbi-button cbi-button-negative',
			'style': 'margin:1px',
			'click': ui.createHandlerFn(this, 'handleStopSection', sid)
		}, _('Stop'));

		ref.btnRestart = E('button', {
			'class': 'cbi-button',
			'style': 'margin:1px',
			'click': ui.createHandlerFn(this, 'handleRestartSection', sid)
		}, _('Restart'));

		ref.btnTest = E('button', {
			'class': 'cbi-button cbi-button-action',
			'style': 'margin:1px',
			'click': ui.createHandlerFn(this, 'handleTestSection', sid)
		}, _('Test'));

		ref.btnLogs = E('button', {
			'class': 'cbi-button',
			'style': 'margin:1px',
			'click': ui.createHandlerFn(this, 'handleLogsSection', sid)
		}, _('Logs'));

		dom.content(ref.actions, [ ref.btnStart, ref.btnStop, ref.btnRestart, ref.btnTest, ref.btnLogs ]);

		ref.tr = E('tr', { 'class': 'tr cbi-section-table-row' }, [
			ref.iface, ref.mode, ref.state, ref.v4, ref.v6, ref.route, ref.actions
		]);

		return ref;
	},

	updateRow: function(c) {
		var ref = this.rowRefs[c.section];
		var routing = [];
		var lastTest;

		if (!ref)
			return;

		dom.content(ref.iface, [
			E('strong', {}, c.section),
			E('div', { 'class': 'cbi-value-description' }, dash(c.iface)),
			c.config_exists ? '' : badge(_('no config'), 'warning'),
			(c.config_exists && !c.config_secure) ? badge(_('insecure file'), 'important') : ''
		]);

		dom.content(ref.mode, badge(c.mode === 'custom' ? _('Custom') : _('Cloudflare'), ''));
		if (!c.enabled)
			dom.content(ref.mode, [ badge(c.mode === 'custom' ? _('Custom') : _('Cloudflare'), ''), ' ', badge(_('disabled'), '') ]);

		if (c.running)
			setBadge(ref.state, _('Running'), 'success');
		else if (c.enabled)
			setBadge(ref.state, _('Stopped'), 'warning');
		else
			setBadge(ref.state, _('Disabled'), '');

		ref.v4.textContent = dash(c.iface_ipv4 || c.warp_ipv4);
		ref.v6.textContent = dash(c.iface_ipv6 || c.warp_ipv6);

		if (c.route4_installed)
			routing.push(badge(_('IPv4 default'), 'success'));
		if (c.route6_installed)
			routing.push(badge(_('IPv6 default'), 'success'));
		if (c.default_route && !c.route4_installed)
			routing.push(badge(_('IPv4 route pending'), 'warning'));
		if (c.ipv6_route === 'always' && !c.route6_installed)
			routing.push(badge(_('IPv6 route pending'), 'warning'));
		if (!c.default_route && !c.route4_installed)
			routing.push(E('span', { 'class': 'cbi-value-description' }, _('IPv4: not routed')));
		if (c.ipv6_route === 'off')
			routing.push(E('span', { 'class': 'cbi-value-description' }, _('IPv6: not routed')));
		if (c.ipv6_route === 'auto' && !c.route6_installed)
			routing.push(E('span', { 'class': 'cbi-value-description' }, _('IPv6: auto (no route needed)')));
		dom.content(ref.route, routing);

		lastTest = this.lastTest[c.section];
		if (lastTest && lastTest.ok === true)
			dom.append(ref.route, E('div', { 'class': 'cbi-value-description' },
				_('egress test: %s %s').format(String(lastTest.warp).toUpperCase(), dash(lastTest.ip))));

		ref.btnStart.disabled = !!c.running;
		ref.btnStop.disabled = !c.running;
		ref.btnRestart.disabled = false;
	},

	updateConnections: function() {
		var self = this;
		var conns = (this.status && this.status.connections) || [];
		var ids = conns.map(function(c) { return c.section; });

		if (JSON.stringify(ids) !== JSON.stringify(this.rowIds || [])) {
			this.rowIds = ids;
			this.rowRefs = {};
			dom.content(this.statusBody, null);
			conns.forEach(function(c) {
				self.rowRefs[c.section] = self.buildRow(c.section);
				dom.append(self.statusBody, self.rowRefs[c.section].tr);
			});
		}
		conns.forEach(function(c) { self.updateRow(c); });
		this.elNoConns.style.display = conns.length ? 'none' : '';
	},

	/* ---------------------------------------------------------------------
	 * configuration form
	 * ------------------------------------------------------------------ */

	buildForm: function(map) {
		var self = this;
		var o, g;

		/* --- global settings ------------------------------------------- */
		var sg = map.section(form.NamedSection, 'settings', 'settings', _('Global settings'));

		o = sg.option(form.Flag, 'lan_ipv6', _('Provide IPv6 to LAN hosts (NAT66 through the tunnel)'));
		o.default = '0';
		o.rmempty = false;
		o.description = _('Announce a ULA /64 on the LAN via router advertisements and NAT66 it through a tunnel. Only effective when a connection is enabled with IPv6 on and ipv6_route=auto or always, i.e. when the tunnel provides the IPv6 default route. The init script sets network.globals.ula_prefix, lan IPv6 addressing and dhcp.lan.ra="server" when needed; disabling it leaves the existing configuration in place.');

		o = sg.option(form.Value, 'lan_ipv6_prefix', _('LAN IPv6 prefix'));
		o.default = '';
		o.optional = true;
		o.datatype = 'cidr6';
		o.placeholder = 'fdd0:1234:5678:1::/64';
		o.description = _('Optional /64 prefix used for the LAN ULA. Empty = derive one from the existing network.globals.ula_prefix (or generate a stable one).');

		/* --- connections ----------------------------------------------- */
		g = map.section(form.GridSection, 'usque', _('Connections'));
		this.grid = g;
		g.anonymous = true;
		g.addremove = true;
		g.sortable = false;
		g.description = _('Each section is one independent tunnel with its own interface, configuration file and state. Routing is opt-in: "Install default route" is OFF by default; the IPv6 default route is only installed by ipv6_route when no other IPv6 default exists (auto) or always.');

		g.sectiontitle = function(sid) {
			var iface = uci.get('usque', sid, 'interface');
			var mode = uci.get('usque', sid, 'mode') || 'cloudflare';
			return (iface || sid) + (mode === 'custom' ? ' (' + _('custom') + ')' : '');
		};

		/* add: pre-fill a unique interface + config file */
		g.handleAdd = function(ev) {
			var config_name = this.uciconfig || this.map.config;
			var sid = this.map.data.add(config_name, this.sectiontype, null);
			var used = {}, next = 0, i, sections, iface;

			sections = this.map.data.sections(config_name, this.sectiontype) || [];
			for (i = 0; i < sections.length; i++) {
				iface = this.map.data.get(config_name, sections[i]['.name'], 'interface');
				if (iface)
					used[iface] = true;
			}
			while (used['warp' + next])
				next++;

			this.map.data.set(config_name, sid, 'interface', 'warp' + next);
			this.map.data.set(config_name, sid, 'config_file', '/etc/usque/warp' + next + '.json');

			var mapNode = this.getPreviousModalMap();
			var prevMap = mapNode ? dom.findClassInstance(mapNode) : this.map;
			prevMap.addedSection = sid;
			return this.renderMoreOptionsModal(sid);
		};

		/* remove: stop the instance, drop the section, commit, restart others */
		g.handleRemove = function(sid) {
			return self.handleRemoveConnection(sid);
		};

		o = g.option(form.Flag, 'enabled', _('Enabled'));
		o.default = '0';
		o.rmempty = false;
		o.width = '6em';

		o = g.option(form.Value, 'interface', _('Interface'));
		o.default = 'warp0';
		o.rmempty = false;
		o.datatype = 'netdevname';
		o.placeholder = 'warp0';
		o.width = '6em';

		o = g.option(form.ListValue, 'mode', _('Mode'));
		o.default = 'cloudflare';
		o.rmempty = false;
		o.value('cloudflare', _('Cloudflare WARP'));
		o.value('custom', _('Custom endpoint'));
		o.width = '9em';

		o = g.option(form.Flag, 'default_route', _('Default route'));
		o.default = '0';
		o.rmempty = false;
		o.width = '5em';
		o.description = _('Install the IPv4 default route via this tunnel (opt-in, default OFF). Disabled: traffic keeps using your WAN and the tunnel is only used by routes/policies you configure. Enabled: endpoint-bypass + default route + policy rule are installed on connect and removed on disconnect.');

		o = g.option(form.ListValue, 'ipv6_route', _('IPv6 route'));
		o.default = 'auto';
		o.rmempty = false;
		o.value('auto', _('auto'));
		o.value('always', _('always'));
		o.value('off', _('off'));
		o.width = '6em';
		o.description = _('auto = install an IPv6 default route through this tunnel only when the router has none (gives IPv6 to IPv4-only/CGNAT WANs). always = always install it (metric 0). off = never.');

		o = g.option(form.Flag, 'ipv6', _('Enable IPv6 on this tunnel'));
		o.default = '1';
		o.rmempty = false;
		o.modalonly = true;

		o = g.option(form.Value, 'metric', _('Default route metric'));
		o.default = '0';
		o.rmempty = false;
		o.datatype = 'uinteger';
		o.placeholder = '0';
		o.modalonly = true;
		o.description = _('Metric of the installed default route (lower wins).');

		o = g.option(form.Value, 'config_file', _('Configuration file'));
		o.default = '/etc/usque/warp0.json';
		o.rmempty = false;
		o.datatype = 'file';
		o.placeholder = '/etc/usque/warp0.json';
		o.modalonly = true;

		o = g.option(form.Value, 'mtu', _('MTU'));
		o.default = '1400';
		o.rmempty = false;
		o.datatype = 'range(576,1500)';
		o.placeholder = '1400';
		o.modalonly = true;

		o = g.option(form.Value, 'endpoint_port', _('Endpoint UDP port'));
		o.default = '443';
		o.rmempty = false;
		o.datatype = 'port';
		o.placeholder = '443';
		o.modalonly = true;

		o = g.option(form.Flag, 'always_reconnect', _('Always reconnect'));
		o.default = '1';
		o.rmempty = false;
		o.modalonly = true;

		o = g.option(form.Flag, 'auto_register', _('Auto-register if the config file is missing'));
		o.default = '1';
		o.rmempty = false;
		o.modalonly = true;
		o.depends('mode', 'cloudflare');

		o = g.option(form.Value, 'device_name', _('Registration device name'));
		o.default = '';
		o.optional = true;
		o.placeholder = _('OpenWrt router');
		o.modalonly = true;
		o.depends('mode', 'cloudflare');

		/* custom-only options: the init script passes -s/--authority/--protocol
		 * whenever they are set, also in cloudflare mode, so clear them when
		 * the connection is not in custom mode */
		var customOnlyWrite = function(option) {
			return function(section_id, formvalue) {
				var mode = this.map.data.get(this.map.config, section_id, 'mode');

				if (mode === 'custom' && formvalue != null && formvalue !== '')
					uci.set(this.map.config, section_id, option, formvalue);
				else
					uci.unset(this.map.config, section_id, option);
			};
		};

		o = g.option(form.Value, 'sni', _('TLS SNI (custom mode)'));
		o.default = '';
		o.optional = true;
		o.placeholder = 'example.com';
		o.modalonly = true;
		o.depends('mode', 'custom');
		o.description = _('TLS server name for non-Cloudflare endpoints (-s). A hostname is fine here; leave empty to use the default (the endpoint IP). Cleared automatically when the mode is not custom.');
		o.write = customOnlyWrite('sni');

		o = g.option(form.Value, 'authority', _('HTTP authority (custom mode)'));
		o.default = '';
		o.optional = true;
		o.placeholder = 'https://cloudflareaccess.com';
		o.modalonly = true;
		o.depends('mode', 'custom');
		o.description = _('CONNECT :authority presented to a non-Cloudflare MASQUE server (--authority). A bare host is normalised to https://<host>. Leave empty for the Cloudflare default. Cleared automatically when the mode is not custom.');
		o.write = customOnlyWrite('authority');

		o = g.option(form.ListValue, 'protocol', _('CONNECT protocol (custom mode)'));
		o.default = '';
		o.optional = true;
		o.rmempty = true;
		o.modalonly = true;
		o.depends('mode', 'custom');
		o.value('', _('Default (cf-connect-ip, Cloudflare)'));
		o.value('connect-ip', _('connect-ip (RFC 9484)'));
		o.description = _('Protocol token for the CONNECT request (--protocol). Leave empty for Cloudflare (cf-connect-ip); pick connect-ip for RFC 9484 servers. Cleared automatically when the mode is not custom.');
		o.write = customOnlyWrite('protocol');

		o = g.option(form.ListValue, 'log_level', _('Log level'));
		o.default = 'info';
		o.rmempty = false;
		o.value('error', _('Error'));
		o.value('warn', _('Warning'));
		o.value('info', _('Info'));
		o.value('debug', _('Debug'));
		o.value('trace', _('Trace (verbose)'));
		o.modalonly = true;

		return map;
	},

	/* ---------------------------------------------------------------------
	 * account / configuration-file section
	 * ------------------------------------------------------------------ */

	renderAccount: function() {
		this.acctSel = E('select', { 'class': 'cbi-input-select' });
		this.acctSel.addEventListener('change', function() {
			this.accountSectionFor = null;
			this.updateAccountSection();
		}.bind(this));

		this.acctBody = E('div', {});

		this.regText = E('span', {}, '');
		this.regOutput = E('pre', {
			'style': 'white-space:pre-wrap; max-height:12em; overflow:auto; font-size:90%; margin-top:0.5em; display:none'
		});
		this.regBox = E('div', { 'style': 'display:none' }, [
			E('div', { 'class': 'cbi-section-node' }, [
				E('div', { 'class': 'spinning' }, this.regText),
				this.regOutput
			])
		]);

		this.accountSectionFor = null;

		return E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('Account / configuration file')),
			E('div', { 'class': 'cbi-value' }, [
				E('label', { 'class': 'cbi-value-title' }, _('Connection')),
				E('div', { 'class': 'cbi-value-field' }, [ this.acctSel ])
			]),
			this.acctBody,
			this.regBox
		]);
	},

	updateAccountSection: function() {
		var conns = (this.status && this.status.connections) || [];
		var ids = conns.map(function(c) { return c.section; });
		var current = this.acctSel.value;

		if (JSON.stringify(ids) !== JSON.stringify(this.acctIds || [])) {
			this.acctIds = ids;
			this.accountSectionFor = null;
			dom.content(this.acctSel, null);
			conns.forEach(function(c) {
				dom.append(this.acctSel, E('option', { 'value': c.section },
					'%s (%s)'.format(c.iface || c.section, c.section)));
			}, this);
			if (ids.indexOf(current) !== -1)
				this.acctSel.value = current;
			else if (ids.length)
				this.acctSel.value = ids[0];
		}

		var sid = this.acctSel.value;
		var conn = sid ? this.connBySection(sid) : null;
		var sig = conn
			? [ sid, conn.config_exists, conn.config_valid, conn.config_secure,
			    conn.config_mode, conn.device_id, conn.mode ].join('|')
			: '';

		if (!sid || (this.accountSectionFor === sid && this.accountSig === sig && !this.accountNeedsUpdate))
			return;

		this.accountSectionFor = sid;
		this.accountSig = sig;
		this.accountNeedsUpdate = false;
		this.renderAccountPanel(sid);
	},

	renderAccountPanel: function(sid) {
		var self = this;
		var conn = this.connBySection(sid);
		var nodes = [];

		if (!conn) {
			dom.content(this.acctBody, E('em', {}, _('No connection selected.')));
			return;
		}

		if (conn.mode === 'custom') {
			nodes.push(E('div', { 'class': 'alert-message notice' }, [
				E('h4', {}, _('Custom endpoint (non-Cloudflare)')),
				E('p', {}, _('Paste a MASQUE/Connect-IP configuration JSON. Required fields: private_key, endpoint_v4, endpoint_pub_key (PEM) and ipv4; optional: ipv6, endpoint_v6. The file is written atomically with mode 600 and contains the private key — treat it as a secret.')),
				E('ul', {}, [
					E('li', {}, _('endpoint_v4 and endpoint_v6 must be IP literals — the configuration file has no DNS field.')),
					E('li', {}, _('Hostnames are only possible through the connection options: TLS SNI (-s) and HTTP authority (--authority), both may be hostnames.')),
					E('li', {}, _('The server certificate is pinned by the SPKI in endpoint_pub_key, so self-signed certificates are fine as long as that key matches.'))
				]),
				E('p', {}, _('Edit the connection (pencil in the table above) to set sni, authority or protocol for custom mode.'))
			]));

			this.importArea = E('textarea', {
				'class': 'cbi-input-textarea',
				'style': 'width:100%; min-height:12em; font-family:monospace; font-size:90%',
				'placeholder': CONFIG_EXAMPLE,
				'spellcheck': 'false'
			});

			nodes.push(valueRow(_('Configuration JSON'), this.importArea,
				_('Required: private_key, endpoint_v4, endpoint_pub_key, ipv4. Optional: ipv6, endpoint_v6.')));
			nodes.push(valueRow(_('Import'), E('div', {}, [
				E('button', {
					'class': 'cbi-button cbi-button-positive important',
					'click': ui.createHandlerFn(this, 'handleImport')
				}, _('Write configuration file'))
			])));
		}
		else {
			/* Cloudflare mode */
			this.regNameInput = E('input', {
				'class': 'cbi-input-text',
				'type': 'text',
				'style': 'width:18em',
				'placeholder': _('OpenWrt router')
			});
			this.regTos = E('input', { 'type': 'checkbox' });

			if (conn.config_exists && conn.config_valid) {
				nodes.push(E('div', { 'class': 'alert-message success' }, [
					E('h4', {}, _('WARP account registered')),
					E('p', {}, _('A configuration file exists for this connection. If the tunnel does not come up, check the log and the device registration.'))
				]));
			}
			else if (conn.config_exists) {
				nodes.push(E('div', { 'class': 'alert-message warning' },
					_('The configuration file exists but is not valid JSON. Delete it and generate/import a new one.')));
			}
			else {
				nodes.push(E('div', { 'class': 'alert-message notice' }, [
					E('h4', {}, _('No WARP configuration yet')),
					E('p', {}, _('Registering creates the configuration file for this connection with a private key and the assigned WARP addresses. Registration talks to Cloudflare directly.'))
				]));
				nodes.push(valueRow(_('Device name'), this.regNameInput,
					_('Optional. Shown on the Cloudflare WARP dashboard; leave empty for the default name.')));
				nodes.push(valueRow(_('Terms of Service'), E('label', {}, [
					this.regTos, ' ',
					_('I accept the '),
					E('a', { 'href': TOS_URL, 'target': '_blank', 'rel': 'noopener' }, _('Cloudflare WARP Terms of Service')),
					'.'
				])));
				nodes.push(valueRow(_('Register'), E('div', {}, [
					E('button', {
						'class': 'cbi-button cbi-button-positive important',
						'click': ui.createHandlerFn(this, 'handleGenerate')
					}, _('Generate WARP configuration'))
				])));
			}
		}

		/* shared config summary */
		var summary = [];
		summary.push(valueRow(_('Configuration file'), [ code(conn.config_file) ]));
		summary.push(valueRow(_('File state'), [
			conn.config_exists
				? badge(_('present'), 'success')
				: badge(_('missing'), 'warning'),
			' ',
			(code(conn.config_mode)),
			conn.config_exists && !conn.config_secure ? E('span', {}, [ ' ', badge(_('readable by other users'), 'important') ]) : ''
		]));

		if (conn.config_exists && conn.config_valid) {
			summary.push(valueRow(_('WARP device ID'), code(conn.device_id)));
			summary.push(valueRow(_('WARP IPv4'), code(conn.warp_ipv4)));
			summary.push(valueRow(_('WARP IPv6'), code(conn.warp_ipv6)));
			summary.push(valueRow(_('Endpoints'), E('span', {},
				dash([ conn.endpoint_v4, conn.endpoint_v6 ].filter(function(v) { return v; }).join(' · ')))));
		}
		else if (conn.config_exists) {
			summary.push(valueRow(_('WARP device ID'), E('em', {}, _('configuration file is not valid JSON'))));
		}

		/* actions */
		var actions = [];
		if (conn.mode === 'cloudflare' && conn.config_exists)
			actions.push(E('button', {
				'class': 'cbi-button cbi-button-reset',
				'click': ui.createHandlerFn(this, 'handleReRegister')
			}, _('Re-register (new device)')));
		if (conn.config_exists)
			actions.push(E('button', {
				'class': 'cbi-button cbi-button-negative',
				'click': ui.createHandlerFn(this, 'handleDeleteConfig')
			}, _('Delete configuration')));
		if (conn.config_exists && !conn.config_secure)
			actions.push(E('button', {
				'class': 'cbi-button cbi-button-apply',
				'click': ui.createHandlerFn(this, 'handleFixPermissions')
			}, _('Fix permissions (chmod 600)')));
		if (actions.length) {
			var wrap = [];
			actions.forEach(function(btn) { wrap.push(btn, ' '); });
			summary.push(valueRow(_('Actions'), E('div', {}, wrap)));
		}

		for (var i = 0; i < summary.length; i++)
			nodes.push(summary[i]);

		dom.content(this.acctBody, E('div', {}, nodes));
	},

	/* ---------------------------------------------------------------------
	 * diagnostics
	 * ------------------------------------------------------------------ */

	renderDiagnostics: function() {
		this.logPre = E('pre', {
			'style': 'white-space:pre-wrap; font-size:90%; max-height:22em; overflow:auto; border:1px solid rgba(127,127,127,0.25); padding:0.4em; margin:0'
		}, (this.logs && this.logs.lines) ? this.logs.lines : _('No usque log entries found.'));
		this.logCount = E('span', { 'class': 'cbi-value-description' },
			this.logs && this.logs.count ? _('%s entries').format(this.logs.count) : '');

		return E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('Diagnostics')),
			E('div', { 'class': 'cbi-map-descr' },
				_('Per-connection egress tests are available in the connection table above (Test / Logs). The log below shows all usque entries.')),
			valueRow(_('System log'), E('div', {}, [
				E('div', {}, [
					E('button', {
						'class': 'cbi-button',
						'click': ui.createHandlerFn(this, 'handleRefreshLogs')
					}, _('Refresh logs')),
					' ', this.logCount
				]),
				this.logPre
			])),
			E('div', { 'class': 'cbi-value' }, [
				E('label', { 'class': 'cbi-value-title' }, _('Documentation')),
				E('div', { 'class': 'cbi-value-field' }, [
					E('a', { 'href': DOCS_URL, 'target': '_blank', 'rel': 'noopener' }, DOCS_URL),
					E('div', { 'class': 'cbi-value-description' },
						_('Installation, UCI options, routing details and troubleshooting.'))
				])
			])
		]);
	},

	/* ---------------------------------------------------------------------
	 * live refresh
	 * ------------------------------------------------------------------ */

	refresh: function() {
		var self = this;
		var tasks;

		if (!this.pollActive)
			return Promise.resolve();

		if (this.rootEl && !document.body.contains(this.rootEl)) {
			this.pollActive = false;
			if (this.pollFn)
				poll.remove(this.pollFn);
			return Promise.resolve();
		}

		tasks = [ L.resolveDefault(callStatus(), null) ];
		if (this.registering)
			tasks.push(L.resolveDefault(callRegisterStatus(this.registering), null));

		return Promise.all(tasks).then(function(res) {
			if (res[0])
				self.updateStatus(res[0]);
			if (self.registering && res[1])
				self.updateRegister(self.registering, res[1]);
		});
	},

	updateStatus: function(st) {
		if (!st || typeof st !== 'object')
			return;

		this.status = st;
		this.updateOverview();
		this.updateConnections();
		this.updateAccountSection();
	},

	connBySection: function(sid) {
		var conns = (this.status && this.status.connections) || [];
		for (var i = 0; i < conns.length; i++)
			if (conns[i].section === sid)
				return conns[i];
		return null;
	},

	/* ---------------------------------------------------------------------
	 * connection actions
	 * ------------------------------------------------------------------ */

	runInitAction: function(action, sid, okMessage) {
		return L.resolveDefault(callSetInitAction(action, sid || ''), null).then(function(res) {
			if (!res || res.ok !== true) {
				ui.addNotification(_('Service control failed'), E('p', {}, errText(res)), 'danger');
			}
			else {
				ui.addNotification(_('usque'),
					E('p', {}, okMessage || _('Action "%s" completed.').format(action)), 'success');
			}
			return null;
		});
	},

	handleStartSection: function(sid) {
		var self = this;
		var conn = this.connBySection(sid);

		if (!conn || !conn.enabled) {
			ui.addNotification(_('Connection is disabled'), E('p', {},
				_('Enable "%s" in the connection table and use Save & Apply first; the init script does not start disabled connections.').format(sid)), 'warning');
			return Promise.resolve();
		}
		if (!conn.config_exists) {
			ui.addNotification(_('No configuration'), E('p', {},
				_('Register a WARP device or import a configuration in the "Account / configuration file" section first; starting now would make the init script register synchronously and block the web interface.')), 'warning');
			return Promise.resolve();
		}

		return this.runInitAction('start', sid, _('Connection "%s" start requested.').format(sid))
			.then(function() { return self.refresh(); });
	},

	handleStopSection: function(sid) {
		var self = this;

		return this.runInitAction('stop', sid, _('Connection "%s" stopped.').format(sid))
			.then(function() { return self.refresh(); });
	},

	handleRestartSection: function(sid) {
		var self = this;
		var conn = this.connBySection(sid);

		if (!conn || !conn.enabled) {
			ui.addNotification(_('Connection is disabled'), E('p', {},
				_('Enable "%s" in the connection table and use Save & Apply first.').format(sid)), 'warning');
			return Promise.resolve();
		}
		if (!conn.config_exists) {
			ui.addNotification(_('No configuration'), E('p', {},
				_('Register a WARP device or import a configuration first.')), 'warning');
			return Promise.resolve();
		}

		return this.runInitAction('restart', sid, _('Connection "%s" restarted.').format(sid))
			.then(function() { return self.refresh(); });
	},

	handleTestSection: function(sid) {
		var self = this;

		return L.resolveDefault(callTest(sid), null).then(function(res) {
			if (!res || res.ok !== true) {
				ui.addNotification(_('Egress test failed — %s').format(sid),
					E('p', {}, errText(res)), 'danger');
			}
			else {
				self.lastTest[sid] = res;
				ui.addNotification(_('Egress test — %s').format(sid),
					E('div', {}, [
						E('p', {}, _('WARP: %s · egress IP: %s · colo: %s').format(
							String(res.warp || 'unknown').toUpperCase(), dash(res.ip), dash(res.colo))),
						E('p', { 'class': 'cbi-value-description' },
							res.bound
								? _('The request was bound to the connection interface.')
								: _('The request was not bound to the tunnel interface; the address shown is the router\'s regular egress address.'))
					]), res.warp === 'on' ? 'success' : 'warning');
			}
			return self.refresh();
		});
	},

	handleLogsSection: function(sid) {
		return L.resolveDefault(callLogs(sid), null).then(function(res) {
			var text = (res && res.ok === true)
				? (res.lines || _('No log entries for this connection.'))
				: errText(res);

			ui.showModal(_('usque log — %s').format(sid), [
				E('pre', {
					'style': 'white-space:pre-wrap; max-height:60vh; overflow:auto; font-size:90%'
				}, text),
				E('div', { 'class': 'right' }, [
					E('button', { 'class': 'cbi-button', 'click': ui.hideModal }, _('Close'))
				])
			]);
		});
	},

	handleRemoveConnection: function(sid) {
		var self = this;

		ui.showModal(_('Remove connection "%s"').format(sid), [
			E('div', {}, [
				E('p', {}, _('The connection is stopped and its UCI section is removed. The configuration file is kept on disk (delete it separately in "Account / configuration file" if you want to remove the WARP registration data).')),
				E('div', { 'class': 'right', 'style': 'margin-top:1em' }, [
					E('button', { 'class': 'cbi-button', 'click': ui.hideModal }, _('Cancel')),
					' ',
					E('button', {
						'class': 'cbi-button cbi-button-negative important',
						'click': function() {
							ui.hideModal();
							var config_name = self.map.config;
							return L.resolveDefault(callSetInitAction('stop', sid), null).then(function() {
								self.map.data.remove(config_name, sid);
								return self.map.save(null, true);
							}).then(function() {
								return L.resolveDefault(callUciApply(10, false), null);
							}).then(function() {
								return L.resolveDefault(callSetInitAction('restart', ''), null);
							}).then(function() {
								ui.addNotification(_('Connection removed'),
									E('p', {}, _('"%s" was removed and the service reloaded.').format(sid)), 'success');
								return self.refresh();
							}).catch(function(err) {
								ui.addNotification(_('Remove failed'), E('p', {}, err.message || String(err)), 'danger');
							});
						}
					}, _('Remove'))
				])
			])
		]);
	},

	/* ---------------------------------------------------------------------
	 * account actions
	 * ------------------------------------------------------------------ */

	handleGenerate: function() {
		var sid = this.acctSel.value;
		var name = (this.regNameInput && this.regNameInput.value || '').trim();

		if (!this.regTos || !this.regTos.checked) {
			ui.addNotification(_('Terms of Service'), E('p', {},
				_('You must accept the Cloudflare WARP Terms of Service before registering a device.')), 'warning');
			return Promise.resolve();
		}

		return this.beginRegistration(sid, name);
	},

	handleReRegister: function() {
		var self = this;
		var sid = this.acctSel.value;
		var nameInput = E('input', {
			'class': 'cbi-input-text', 'type': 'text', 'style': 'width:18em',
			'placeholder': _('OpenWrt router')
		});
		var tos = E('input', { 'type': 'checkbox' });
		var alertBox = E('div', { 'class': 'alert-message warning', 'style': 'display:none' });

		ui.showModal(_('Re-register "%s"').format(sid), [
			E('div', {}, [
				E('p', {}, _('A new WARP device will be registered with Cloudflare and the configuration file of this connection will be overwritten. Repeated registrations may count against the WARP device limit of the account.')),
				valueRow(_('Device name'), nameInput, _('Optional.')),
				E('label', {}, [
					tos, ' ',
					_('I accept the '),
					E('a', { 'href': TOS_URL, 'target': '_blank', 'rel': 'noopener' }, _('Cloudflare WARP Terms of Service')),
					'.'
				]),
				alertBox,
				E('div', { 'class': 'right', 'style': 'margin-top:1em' }, [
					E('button', { 'class': 'cbi-button', 'click': ui.hideModal }, _('Cancel')),
					' ',
					E('button', {
						'class': 'cbi-button cbi-button-negative important',
						'click': function() {
							if (!tos.checked) {
								alertBox.style.display = '';
								alertBox.textContent = _('Please accept the Terms of Service first.');
								return;
							}
							ui.hideModal();
							return self.beginRegistration(sid, (nameInput.value || '').trim());
						}
					}, _('Re-register'))
				])
			])
		]);
	},

	handleImport: function() {
		var self = this;
		var sid = this.acctSel.value;
		var text = (this.importArea && this.importArea.value || '').trim();

		if (!text) {
			ui.addNotification(_('Nothing to import'), E('p', {}, _('Paste a configuration JSON first.')), 'warning');
			return Promise.resolve();
		}
		if (text.charAt(0) !== '{') {
			ui.addNotification(_('Invalid configuration'), E('p', {}, _('The configuration must be a JSON object starting with "{" and ending with "}".')), 'warning');
			return Promise.resolve();
		}

		return L.resolveDefault(callImportConfig(sid, text), null).then(function(res) {
			if (!res || res.ok !== true) {
				ui.addNotification(_('Import failed'), E('p', {}, errText(res)), 'danger');
				return;
			}
			if (self.importArea)
				self.importArea.value = '';
			ui.addNotification(_('Configuration imported'),
				E('p', {}, _('The configuration file for "%s" was written with mode 600.').format(sid)), 'success');
			self.accountNeedsUpdate = true;
			return self.refresh();
		});
	},

	handleDeleteConfig: function() {
		var self = this;
		var sid = this.acctSel.value;

		ui.showModal(_('Delete configuration of "%s"').format(sid), [
			E('div', {}, [
				E('p', {}, _('This removes the configuration file and stops the connection. The device registration itself remains on the Cloudflare side; registering again creates a new device.')),
				E('div', { 'class': 'right', 'style': 'margin-top:1em' }, [
					E('button', { 'class': 'cbi-button', 'click': ui.hideModal }, _('Cancel')),
					' ',
					E('button', {
						'class': 'cbi-button cbi-button-negative important',
						'click': function() {
							ui.hideModal();
							return L.resolveDefault(callDeleteConfig(sid), null).then(function(res) {
								if (!res || res.ok !== true)
									ui.addNotification(_('Delete failed'), E('p', {}, errText(res)), 'danger');
								else
									ui.addNotification(_('Configuration deleted'),
										E('p', {}, _('The configuration file was removed and the connection stopped.')), 'success');
								self.accountNeedsUpdate = true;
								return self.refresh();
							});
						}
					}, _('Delete'))
				])
			])
		]);
	},

	handleFixPermissions: function() {
		var self = this;
		var sid = this.acctSel.value;

		return L.resolveDefault(callFixPermissions(sid), null).then(function(res) {
			if (!res || res.ok !== true)
				ui.addNotification(_('chmod failed'), E('p', {}, errText(res)), 'danger');
			else
				ui.addNotification(_('Permissions fixed'),
					E('p', {}, _('The configuration file is now readable by root only.')), 'success');
			self.accountNeedsUpdate = true;
			return self.refresh();
		});
	},

	handleRefreshLogs: function() {
		var self = this;

		return L.resolveDefault(callLogs(''), null).then(function(res) {
			if (!res || res.ok !== true) {
				self.logPre.textContent = errText(res);
				self.logCount.textContent = '';
				return;
			}
			self.logPre.textContent = res.lines || _('No usque log entries found.');
			self.logCount.textContent = res.count ? _('%s entries').format(res.count) : '';
		});
	},

	beginRegistration: function(sid, name) {
		var self = this;

		if (!sid) {
			ui.addNotification(_('No connection selected'), E('p', {}, _('Select a connection first.')), 'warning');
			return Promise.resolve();
		}

		this.registering = sid;
		this.regT0 = Date.now();
		this.regText.textContent = _('Requesting a WARP registration from Cloudflare…');
		this.regOutput.textContent = '';
		this.regOutput.style.display = 'none';
		this.regBox.style.display = '';

		return L.resolveDefault(callRegister(sid, name, true), null).then(function(res) {
			if (!res || res.ok !== true) {
				self.registering = null;
				self.regBox.style.display = 'none';
				ui.addNotification(_('Registration failed'), E('p', {}, errText(res)), 'danger');
				return;
			}
			self.regText.textContent = _('Registering with Cloudflare…');
			return self.refresh();
		});
	},

	updateRegister: function(sid, res) {
		var elapsed;

		if (!this.registering || this.registering !== sid)
			return;

		if (!res || res.ok !== true) {
			this.registering = null;
			this.regBox.style.display = 'none';
			ui.addNotification(_('Registration failed'), E('p', {}, errText(res)), 'danger');
			return;
		}

		elapsed = res.elapsed > 0 ? res.elapsed : Math.round((Date.now() - (this.regT0 || Date.now())) / 1000);

		if (!res.done) {
			this.regText.textContent = _('Registering "%s" with Cloudflare… %s').format(sid, fmtDuration(elapsed));
			if (res.output) {
				this.regOutput.style.display = '';
				this.regOutput.textContent = res.output;
			}
			return;
		}

		this.registering = null;

		if (res.rc === 0) {
			this.regBox.style.display = 'none';
			ui.addNotification(_('WARP configuration created'),
				E('p', {}, _('Enable the connection in the table and press Save & Apply, then Start.')), 'success');
			this.accountNeedsUpdate = true;
			return this.refresh();
		}

		this.regText.textContent = res.timed_out
			? _('Registration timed out.')
			: _('Registration failed (exit code %s).').format(dash(res.rc));
		this.regBox.style.display = '';
		if (res.output) {
			this.regOutput.style.display = '';
			this.regOutput.textContent = res.output;
		}
		ui.addNotification(_('Registration failed'), E('p', {},
			_('See the output in the "Account / configuration file" section.')), 'danger');
		return this.refresh();
	},

	/* ---------------------------------------------------------------------
	 * save / apply
	 * ------------------------------------------------------------------ */

	handleSaveApply: function(ev) {
		var self = this;

		/* 1. stage the form values in the uci session (View.handleSave)
		 * 2. commit them (uci apply) so the init script sees them
		 * 3. sync the boot symlink and restart the service
		 * 4. drop the "unsaved changes" indicator */
		return this.super('handleSave', [ ev ])
			.then(function() { return callUciApply(10, false); })
			.then(function() { return self.applyServiceState(); })
			.then(function() {
				if (ui.changes && typeof(ui.changes.setIndicator) === 'function')
					ui.changes.setIndicator(0);
			})
			.catch(function(err) {
				ui.addNotification(_('Save failed'), E('p', {}, err.message || String(err)), 'danger');
				return self.refresh();
			});
	},

	applyServiceState: function() {
		var self = this;
		var sections = uci.sections('usque', 'usque') || [];
		var anyEnabled = false, missingConfig = [], conn;

		sections.forEach(function(s) {
			if (parseBool(s.enabled))
				anyEnabled = true;
		});

		/* connections that are enabled but still lack a config file are skipped:
		 * the init script would register them synchronously and block this RPC */
		for (var i = 0; i < sections.length; i++) {
			if (!parseBool(sections[i].enabled))
				continue;
			conn = this.connBySection(sections[i]['.name']);
			if (conn && conn.config_exists === false)
				missingConfig.push(sections[i]['.name']);
		}

		var tasks = [];
		tasks.push(callSetInitAction(anyEnabled ? 'enable' : 'disable', ''));
		if (anyEnabled && !missingConfig.length)
			tasks.push(callSetInitAction('restart', ''));

		return tasks.reduce(function(chain, task) {
			return chain.then(function() {
				return L.resolveDefault(task, null).then(function(res) {
					if (!res || res.ok !== true)
						throw new Error(errText(res));
				});
			});
		}, Promise.resolve()).then(function() {
			if (anyEnabled && missingConfig.length)
				ui.addNotification(_('Configuration saved'), E('p', {},
					_('Connection(s) %s are enabled but have no configuration yet: register or import a configuration first; the service was not restarted to avoid blocking the web interface.').format(missingConfig.join(', '))), 'warning');
			else if (!anyEnabled)
				ui.addNotification(_('Configuration saved'), E('p', {},
					_('No connection is enabled, the usque service was stopped and disabled.')), 'success');
			else
				ui.addNotification(_('Configuration saved'), E('p', {},
					_('usque was restarted with the new configuration.')), 'success');
			return self.refresh();
		}).catch(function(err) {
			ui.addNotification(_('Service control failed'), E('p', {}, err.message || String(err)), 'danger');
		});
	}
});
