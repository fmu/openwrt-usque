'use strict';
'require view';
'require form';
'require rpc';
'require poll';
'require ui';
'require uci';
'require dom';

/*
 * LuCI view for luci-app-usque: Services -> usque (Cloudflare WARP)
 *
 * The heavy lifting happens in the rpcd backend /usr/libexec/rpcd/luci.usque
 * (object luci.usque). This view only renders state and triggers actions.
 */

var DOCS_URL = 'https://fmu.github.io/openwrt-usque/';
var TOS_URL = 'https://www.cloudflare.com/application/terms/';
var POLL_INTERVAL = 5;

/* rpcd methods (see /usr/libexec/rpcd/luci.usque) */
var callStatus = rpc.declare({ object: 'luci.usque', method: 'status', expect: {} });
var callGetConfig = rpc.declare({ object: 'luci.usque', method: 'getConfig', expect: {} });
var callRegister = rpc.declare({ object: 'luci.usque', method: 'register', params: [ 'name', 'accept_tos' ], expect: {} });
var callRegisterStatus = rpc.declare({ object: 'luci.usque', method: 'registerStatus', expect: {} });
var callSetInitAction = rpc.declare({ object: 'luci.usque', method: 'setInitAction', params: [ 'action' ], expect: {} });
var callTest = rpc.declare({ object: 'luci.usque', method: 'test', expect: {} });
var callLogs = rpc.declare({ object: 'luci.usque', method: 'logs', expect: {} });
var callDeleteConfig = rpc.declare({ object: 'luci.usque', method: 'deleteConfig', expect: {} });
var callFixPermissions = rpc.declare({ object: 'luci.usque', method: 'fixConfigPermissions', expect: {} });

/* Map.save() only stages changes in the uci session delta; the init script reads
 * the committed /etc/config/usque, so we have to commit (apply) before restarting.
 * "uci apply" is part of the standard LuCI ACLs, plain "uci commit" is not. */
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
	var node = E('span', { 'class': 'label' + (cls ? ' ' + cls : '') }, text);
	return node;
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

/* -------------------------------------------------------------------------
 * view
 * ---------------------------------------------------------------------- */

return view.extend({
	load: function() {
		return Promise.all([
			L.resolveDefault(callStatus(), null),
			L.resolveDefault(callGetConfig(), null),
			L.resolveDefault(callLogs(), null)
		]);
	},

	render: function(data) {
		var self = this;

		this.status = (data && data[0]) || {};
		this.config = (data && data[1]) || {};
		this.logs = (data && data[2]) || {};
		this.lastTest = null;
		this.registering = false;
		this.pollActive = true;
		this.pollFn = null;

		var statusSection = this.renderStatusSection();
		var accountSection = this.renderAccountSection();
		var diagSection = this.renderDiagnosticsSection();

		var map = new form.Map('usque', '', _('Requires UDP 443 to Cloudflare. While the tunnel is up, policy routing/mwan3 rules for the default route are overridden. These options are written to /etc/config/usque; Save & Apply commits them and restarts the service.'));
		this.map = map;
		this.buildForm(map);

		return map.render().then(function(mapNode) {
			self.rootEl = E('div', {}, [ statusSection, accountSection, mapNode, diagSection ]);
			self.updateStatus(self.status);
			self.updateAccount(self.status);

			self.pollFn = function() { return self.refresh(); };
			poll.add(self.pollFn, POLL_INTERVAL);

			return self.rootEl;
		}).catch(function(err) {
			/* /etc/config/usque missing or ACL problem: keep the page usable */
			self.rootEl = E('div', {}, [
				statusSection,
				E('div', { 'class': 'cbi-section' }, [
					E('div', { 'class': 'alert-message danger' }, [
						E('h4', {}, _('Cannot load the usque configuration')),
						E('p', {}, _('Reading /etc/config/usque failed: %s').format(err.message || err)),
						E('p', {}, _('Make sure the usque package is installed, then reload this page.'))
					])
				]),
				accountSection,
				diagSection
			]);
			self.updateStatus(self.status);
			self.updateAccount(self.status);
			return self.rootEl;
		});
	},

	/* ---------------------------------------------------------------------
	 * status section
	 * ------------------------------------------------------------------ */

	renderStatusSection: function() {
		this.elBackend = E('div', { 'class': 'alert-message danger', 'style': 'display:none' });
		this.elService = badge(_('unknown'));
		this.elServiceNote = E('span', { 'class': 'cbi-value-description' });
		this.elTunnel = badge(_('unknown'));
		this.elIface = E('span');
		this.elConfig = code();
		this.elConfigPerm = E('span', { 'class': 'cbi-value-description' });
		this.elDevice = code();
		this.elWarpV4 = code();
		this.elWarpV6 = code();
		this.elEndpoint = E('span');
		this.elEgress = E('span', {}, _('not tested yet'));

		this.btnStart = E('button', {
			'class': 'cbi-button cbi-button-positive',
			'click': ui.createHandlerFn(this, 'handleStart')
		}, _('Start'));

		this.btnStop = E('button', {
			'class': 'cbi-button cbi-button-negative',
			'click': ui.createHandlerFn(this, 'handleStop')
		}, _('Stop'));

		this.btnRestart = E('button', {
			'class': 'cbi-button',
			'click': ui.createHandlerFn(this, 'handleRestart')
		}, _('Restart'));

		this.btnTest = E('button', {
			'class': 'cbi-button cbi-button-action',
			'click': ui.createHandlerFn(this, 'handleTest')
		}, _('Test connection'));

		return E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('Status')),
			this.elBackend,
			valueRow(_('Service'), [ this.elService, ' ', this.elServiceNote ]),
			valueRow(_('Tunnel'), [ this.elTunnel, ' ', this.elIface ]),
			valueRow(_('Configuration'), [ this.elConfig, ' ', this.elConfigPerm ]),
			valueRow(_('WARP device'), [ this.elDevice ]),
			valueRow(_('WARP IPv4'), [ this.elWarpV4 ]),
			valueRow(_('WARP IPv6'), [ this.elWarpV6 ]),
			valueRow(_('Endpoints'), [ this.elEndpoint ]),
			valueRow(_('Egress test'), [ this.elEgress, ' ', this.btnTest ]),
			valueRow(_('Service control'), E('div', {}, [ this.btnStart, ' ', this.btnStop, ' ', this.btnRestart ]),
				_('Start/Stop act on /etc/init.d/usque immediately. The configuration below is applied with Save & Apply, which also restarts the service.'))
		]);
	},

	updateStatus: function(st) {
		var running, enabled, iface, extras, endpoints;

		if (!st || typeof st !== 'object') {
			this.elBackend.style.display = '';
			this.elBackend.textContent = _('The rpcd backend did not answer.');
			return;
		}

		/* keep the latest state around for the action handlers */
		this.status = st;

		if (st.ok !== true) {
			this.elBackend.style.display = '';
			this.elBackend.textContent = _('Backend error: %s').format(errText(st));
		}
		else {
			this.elBackend.style.display = 'none';
		}

		running = !!st.running;
		enabled = !!st.enabled;

		if (running)
			setBadge(this.elService, _('Running'), 'success');
		else if (enabled)
			setBadge(this.elService, _('Stopped'), 'warning');
		else
			setBadge(this.elService, _('Disabled'), '');

		if (running) {
			if (st.uptime > 0)
				this.elServiceNote.textContent = _('pid %s, up %s').format(dash(st.pid), fmtDuration(st.uptime));
			else if (st.pid)
				this.elServiceNote.textContent = _('pid %s').format(st.pid);
			else
				this.elServiceNote.textContent = '';
		}
		else if (!st.installed) {
			this.elServiceNote.textContent = _('usque binary not installed');
		}
		else if (!st.init_script) {
			this.elServiceNote.textContent = _('init script /etc/init.d/usque missing');
		}
		else if (enabled && !st.init_enabled) {
			this.elServiceNote.textContent = _('enabled in the configuration but not enabled on boot \u2014 Save & Apply fixes this');
		}
		else if (!enabled) {
			this.elServiceNote.textContent = _('disabled in the configuration (enabled=0), the service will not start');
		}
		else {
			this.elServiceNote.textContent = '';
		}

		if (!st.iface_exists)
			setBadge(this.elTunnel, _('no tunnel'), '');
		else if (st.iface_up)
			setBadge(this.elTunnel, _('up'), 'success');
		else
			setBadge(this.elTunnel, _('down'), 'warning');

		iface = dash(st.iface);
		extras = [];
		if (st.iface_ipv4)
			extras.push(st.iface_ipv4);
		if (st.iface_ipv6)
			extras.push(st.iface_ipv6);
		if (st.default_route_via_iface)
			extras.push(_('default route'));
		this.elIface.textContent = iface + (extras.length ? ' — ' + extras.join(', ') : '');

		if (!st.config_exists)
			this.elConfig.textContent = _('%s (missing)').format(dash(st.config_path));
		else if (!st.config_valid)
			this.elConfig.textContent = _('%s (invalid JSON)').format(dash(st.config_path));
		else
			this.elConfig.textContent = dash(st.config_path);

		if (st.config_exists && !st.config_secure)
			setBadge(this.elConfigPerm, _('mode %s — private key is readable by other users!').format(dash(st.config_mode)), 'important');
		else if (st.config_exists && st.config_mode) {
			this.elConfigPerm.className = 'cbi-value-description';
			this.elConfigPerm.textContent = _('mode %s').format(st.config_mode);
		}
		else {
			this.elConfigPerm.className = 'cbi-value-description';
			this.elConfigPerm.textContent = '';
		}

		this.elDevice.textContent = dash(st.device_id);
		this.elWarpV4.textContent = dash(st.warp_ipv4);
		this.elWarpV6.textContent = dash(st.warp_ipv6);

		endpoints = [];
		if (st.endpoint_v4)
			endpoints.push(st.endpoint_v4);
		if (st.endpoint_v6)
			endpoints.push(st.endpoint_v6);
		this.elEndpoint.textContent = endpoints.length ? endpoints.join(' · ') : '—';

		if (this.lastTest) {
			this.elEgress.textContent = _('WARP %s · %s · colo %s').format(
				String(this.lastTest.warp || 'unknown').toUpperCase(),
				dash(this.lastTest.ip), dash(this.lastTest.colo));
		}

		this.updateAccount(st);
	},

	/* ---------------------------------------------------------------------
	 * WARP account section
	 * ------------------------------------------------------------------ */

	renderAccountSection: function() {
		var self = this;

		this.regNameInput = E('input', {
			'class': 'cbi-input-text',
			'type': 'text',
			'style': 'width:18em',
			'placeholder': _('OpenWrt router')
		});

		this.regTos = E('input', { 'type': 'checkbox' });

		this.btnGenerate = E('button', {
			'class': 'cbi-button cbi-button-positive important',
			'click': ui.createHandlerFn(this, 'handleRegister')
		}, _('Generate WARP configuration'));

		this.cfgWarn = E('div', { 'class': 'alert-message warning', 'style': 'display:none' });
		this.btnDeleteInvalid = E('button', {
			'class': 'cbi-button cbi-button-negative',
			'style': 'display:none',
			'click': ui.createHandlerFn(this, 'handleDeleteConfig')
		}, _('Delete configuration file'));

		this.noCfgBox = E('div', {}, [
			E('div', { 'class': 'alert-message notice' }, [
				E('h4', {}, _('No WARP configuration')),
				E('p', {}, _('usque needs a WARP account. Registering creates /etc/usque/warp0.json with a private key and the assigned WARP addresses. Registration talks to Cloudflare directly and takes a few seconds.'))
			]),
			this.cfgWarn,
			E('div', { 'style': 'margin:0.5em 0' }, [ this.btnDeleteInvalid ]),
			valueRow(_('Device name'), this.regNameInput,
				_('Optional. Shown on the Cloudflare WARP dashboard; leave empty to let usque pick a default name.')),
			valueRow(_('Terms of Service'), E('label', {}, [
				this.regTos, ' ',
				_('I accept the '),
				E('a', { 'href': TOS_URL, 'target': '_blank', 'rel': 'noopener' }, _('Cloudflare WARP Terms of Service')),
				'.'
			])),
			valueRow('', E('div', {}, [ this.btnGenerate ]),
				_('The registration is done by usque itself; the WARP private key never leaves the router and is never shown in this interface.'))
		]);

		this.accId = code();
		this.accV4 = code();
		this.accV6 = code();
		this.accEndpoint = E('span');
		this.accPath = code();
		this.accPerm = E('span', { 'class': 'cbi-value-description' });
		this.btnFixPerms = E('button', {
			'class': 'cbi-button cbi-button-apply',
			'style': 'display:none',
			'click': ui.createHandlerFn(this, 'handleFixPermissions')
		}, _('Fix permissions (chmod 600)'));

		this.btnReRegister = E('button', {
			'class': 'cbi-button cbi-button-reset',
			'click': ui.createHandlerFn(this, 'handleReRegister')
		}, _('Re-register (new device)'));

		this.btnDelete = E('button', {
			'class': 'cbi-button cbi-button-negative',
			'click': ui.createHandlerFn(this, 'handleDeleteConfig')
		}, _('Delete configuration'));

		this.hasCfgBox = E('div', { 'style': 'display:none' }, [
			valueRow(_('WARP device ID'), [ this.accId ]),
			valueRow(_('WARP IPv4'), [ this.accV4 ]),
			valueRow(_('WARP IPv6'), [ this.accV6 ]),
			valueRow(_('Endpoints'), [ this.accEndpoint ]),
			valueRow(_('Configuration file'), [ this.accPath, ' ', this.accPerm, ' ', this.btnFixPerms ]),
			valueRow(_('Account actions'), E('div', {}, [ this.btnReRegister, ' ', this.btnDelete ]),
				_('Re-registering registers a new device with Cloudflare and overwrites the configuration file. Deleting the configuration stops the service and removes the file.'))
		]);

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

		return E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('WARP account')),
			this.noCfgBox,
			this.hasCfgBox,
			this.regBox
		]);
	},

	updateAccount: function(st) {
		var hasFile, valid, usable, endpoints;

		if (!st || typeof st !== 'object')
			return;

		hasFile = !!st.config_exists;
		valid = !!st.config_valid;
		usable = hasFile && valid;

		this.noCfgBox.style.display = usable ? 'none' : '';
		this.hasCfgBox.style.display = usable ? '' : 'none';

		if (hasFile && !valid) {
			this.cfgWarn.style.display = '';
			this.cfgWarn.textContent = _('A configuration file exists at %s but it is not valid JSON. Re-generate the configuration or delete the broken file.').format(dash(st.config_path));
			this.btnDeleteInvalid.style.display = '';
		}
		else {
			this.cfgWarn.style.display = 'none';
			this.btnDeleteInvalid.style.display = 'none';
		}

		if (!usable)
			return;

		this.accId.textContent = dash(st.device_id);
		this.accV4.textContent = dash(st.warp_ipv4);
		this.accV6.textContent = dash(st.warp_ipv6);
		this.accPath.textContent = dash(st.config_path);

		endpoints = [];
		if (st.endpoint_v4)
			endpoints.push(st.endpoint_v4);
		if (st.endpoint_v6)
			endpoints.push(st.endpoint_v6);
		this.accEndpoint.textContent = endpoints.length ? endpoints.join(' · ') : '—';

		if (!st.config_secure) {
			setBadge(this.accPerm, _('permissions %s — the WARP private key is readable by other users').format(dash(st.config_mode)), 'important');
			this.btnFixPerms.style.display = '';
		}
		else {
			this.accPerm.className = 'cbi-value-description';
			this.accPerm.textContent = '';
			this.btnFixPerms.style.display = 'none';
		}
	},

	/* ---------------------------------------------------------------------
	 * configuration form
	 * ------------------------------------------------------------------ */

	buildForm: function(map) {
		var s = map.section(form.NamedSection, 'main', 'usque', _('Configuration'));
		var o;

		o = s.option(form.Flag, 'enabled', _('Enable usque service'));
		o.default = '0';
		o.rmempty = false;
		o.description = _('Master switch: the init script only starts usque when this is checked. Save & Apply also updates the boot symlink (/etc/init.d/usque enable) and restarts the service.');

		o = s.option(form.Flag, 'auto_register', _('Auto-register if no configuration exists'));
		o.default = '1';
		o.rmempty = false;
		o.description = _('The init script registers a WARP account on start if the configuration file is missing.');

		o = s.option(form.Value, 'device_name', _('Registration device name'));
		o.default = '';
		o.optional = true;
		o.placeholder = _('OpenWrt router');
		o.description = _('Used by the auto-registration on boot. Leave empty for the default name.');

		o = s.option(form.Value, 'config_file', _('Configuration file'));
		o.default = '/etc/usque/warp0.json';
		o.rmempty = false;
		o.datatype = 'file';
		o.placeholder = '/etc/usque/warp0.json';
		o.description = _('WARP configuration written by the registration (contains the private key, mode 600).');

		o = s.option(form.Value, 'interface', _('Tunnel interface'));
		o.default = 'warp0';
		o.rmempty = false;
		o.datatype = 'maxlength(15)';
		o.placeholder = 'warp0';
		o.description = _('Name of the TUN interface created by usque (max. 15 characters, letters, digits, ".", "_", "-").');

		o = s.option(form.Value, 'mtu', _('MTU'));
		o.default = '1400';
		o.rmempty = false;
		o.datatype = 'range(576,1500)';
		o.placeholder = '1400';

		o = s.option(form.Value, 'endpoint_port', _('Endpoint port'));
		o.default = '443';
		o.rmempty = false;
		o.datatype = 'port';
		o.placeholder = '443';
		o.description = _('UDP port used for MASQUE. Some networks only allow UDP/443.');

		o = s.option(form.Flag, 'ipv6', _('Enable IPv6'));
		o.default = '0';
		o.rmempty = false;
		o.description = _('Assign and route the WARP IPv6 address (only useful if the WAN has IPv6).');

		o = s.option(form.Flag, 'always_reconnect', _('Always reconnect'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.ListValue, 'log_level', _('Log level'));
		o.default = 'info';
		o.rmempty = false;
		o.value('error', _('Error'));
		o.value('warn', _('Warning'));
		o.value('info', _('Info'));
		o.value('debug', _('Debug'));
		o.value('trace', _('Trace (verbose)'));
		o.description = _('Passed to usque as RUST_LOG. "debug" or "trace" is only recommended for troubleshooting.');

		return s;
	},

	/* ---------------------------------------------------------------------
	 * diagnostics section
	 * ------------------------------------------------------------------ */

	renderDiagnosticsSection: function() {
		this.btnTestDiag = E('button', {
			'class': 'cbi-button cbi-button-action',
			'click': ui.createHandlerFn(this, 'handleTest')
		}, _('Run egress test'));

		this.btnRefreshLogs = E('button', {
			'class': 'cbi-button',
			'click': ui.createHandlerFn(this, 'handleRefreshLogs')
		}, _('Refresh logs'));

		this.testResult = E('div', {}, [ E('em', {}, _('No test has been run yet.')) ]);

		this.logCount = E('span', { 'class': 'cbi-value-description' },
			this.logs && this.logs.count ? _('%s entries').format(this.logs.count) : '');

		this.logPre = E('pre', {
			'style': 'white-space:pre-wrap; font-size:90%; max-height:22em; overflow:auto; border:1px solid rgba(127,127,127,0.25); padding:0.4em; margin:0'
		}, (this.logs && this.logs.lines) ? this.logs.lines : _('No usque log entries found.'));

		return E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('Diagnostics')),
			valueRow(_('Connection test'), E('div', {}, [ this.btnTestDiag ]),
				_('Fetches https://www.cloudflare.com/cdn-cgi/trace through the tunnel when possible and reports the Cloudflare view of the connection (warp=on means the traffic really goes through WARP).')),
			valueRow('', this.testResult),
			valueRow(_('Log'), E('div', {}, [ E('div', {}, [ this.btnRefreshLogs, ' ', this.logCount ]), this.logPre ]),
				_('Last entries matching "usque" from the system log (logread).')),
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
	 * actions
	 * ------------------------------------------------------------------ */

	refresh: function() {
		var self = this;

		if (!this.pollActive)
			return Promise.resolve();

		if (this.rootEl && !document.body.contains(this.rootEl)) {
			this.pollActive = false;
			if (this.pollFn)
				poll.remove(this.pollFn);
			return Promise.resolve();
		}

		var tasks = [ L.resolveDefault(callStatus(), null) ];
		if (this.registering)
			tasks.push(L.resolveDefault(callRegisterStatus(), null));

		return Promise.all(tasks).then(function(res) {
			if (res[0])
				self.updateStatus(res[0]);
			if (self.registering && res[1])
				self.updateRegister(res[1]);
		});
	},

	handleStart: function() {
		if (this.status && this.status.enabled !== true) {
			ui.addNotification(_('Service is disabled'), E('p', {},
				_('usque.main.enabled is 0, so /etc/init.d/usque will not start the tunnel. Enable "Enable usque service" above and press Save & Apply.')), 'warning');
			return Promise.resolve();
		}

		return this.doServiceAction('start');
	},

	handleStop: function() {
		return this.doServiceAction('stop');
	},

	handleRestart: function() {
		return this.doServiceAction('restart');
	},

	doServiceAction: function(action) {
		var self = this;

		return L.resolveDefault(callSetInitAction(action), null).then(function(res) {
			if (!res || res.ok !== true)
				ui.addNotification(_('Service control failed'), E('p', {}, errText(res)), 'danger');
			else
				ui.addNotification(_('usque'), E('p', {}, _('Service action "%s" completed.').format(action)), 'success');

			return self.refresh();
		});
	},

	handleTest: function() {
		var self = this;
		var iface = dash(this.status && this.status.iface);

		dom.content(this.testResult, E('div', { 'class': 'spinning' },
			_('Running egress test through %s…').format(iface)));

		return L.resolveDefault(callTest(), null).then(function(res) {
			if (res && res.ok === true)
				self.lastTest = res;
			self.renderTestResult(res);
			return self.refresh();
		});
	},

	renderTestResult: function(res) {
		var nodes = [], warpCls;

		if (!res || res.ok !== true) {
			dom.content(this.testResult, E('div', { 'class': 'alert-message danger' }, [
				E('strong', {}, _('Test failed')),
				E('p', {}, errText(res))
			]));
			return;
		}

		warpCls = res.warp === 'on' ? 'success' : (res.warp === 'off' ? 'warning' : '');

		nodes.push(valueRow(_('WARP'), badge(String(res.warp || 'unknown').toUpperCase(), warpCls)));
		nodes.push(valueRow(_('Egress IP'), code(res.ip)));
		nodes.push(valueRow(_('Cloudflare colo'), code(res.colo)));
		nodes.push(valueRow(_('Location'), code(res.loc)));
		nodes.push(valueRow(_('Timestamp'), code(res.timestamp)));
		nodes.push(valueRow(_('Test method'), E('span', {},
			dash(res.method) + (res.bound ? _(' (bound to the tunnel)') : _(' (not bound to the tunnel)')))));

		if (!res.bound) {
			nodes.push(E('div', { 'class': 'alert-message notice' },
				_('The request could not be bound to the tunnel interface, so the address shown is the router\'s regular egress address. Start the service and make sure the default route uses the tunnel for a tunnel-bound test.')));
		}

		if (res.raw) {
			nodes.push(E('details', {}, [
				E('summary', {}, _('Raw trace')),
				E('pre', { 'style': 'white-space:pre-wrap' }, res.raw)
			]));
		}

		dom.content(this.testResult, E('div', {}, nodes));
	},

	handleRefreshLogs: function() {
		var self = this;

		return L.resolveDefault(callLogs(), null).then(function(res) {
			if (!res || res.ok !== true) {
				self.logPre.textContent = errText(res);
				self.logCount.textContent = '';
				return;
			}
			self.logPre.textContent = res.lines || _('No usque log entries found.');
			self.logCount.textContent = res.count ? _('%s entries').format(res.count) : '';
		});
	},

	handleFixPermissions: function() {
		var self = this;

		return L.resolveDefault(callFixPermissions(), null).then(function(res) {
			if (!res || res.ok !== true)
				ui.addNotification(_('chmod failed'), E('p', {}, errText(res)), 'danger');
			else
				ui.addNotification(_('Permissions fixed'), E('p', {},
					_('The configuration file is now readable by root only.')), 'success');
			return self.refresh();
		});
	},

	handleRegister: function() {
		var name = (this.regNameInput.value || '').trim();

		if (!this.regTos.checked) {
			ui.addNotification(_('Terms of Service'), E('p', {},
				_('You must accept the Cloudflare WARP Terms of Service before registering a device.')), 'warning');
			return;
		}

		return this.beginRegistration(name);
	},

	handleReRegister: function() {
		var self = this;
		var nameInput = E('input', {
			'class': 'cbi-input-text',
			'type': 'text',
			'style': 'width:18em',
			'placeholder': _('OpenWrt router')
		});
		var tos = E('input', { 'type': 'checkbox' });
		var alertBox = E('div', { 'class': 'alert-message warning', 'style': 'display:none' });

		ui.showModal(_('Re-register this device'), [
			E('div', {}, [
				E('p', {}, _('A new WARP device will be registered with Cloudflare and the existing configuration file will be overwritten. Repeated registrations may count against the WARP device limit of the account.')),
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
							self.beginRegistration((nameInput.value || '').trim());
						}
					}, _('Re-register'))
				])
			])
		]);
	},

	handleDeleteConfig: function() {
		var self = this;

		ui.showModal(_('Delete WARP configuration'), [
			E('div', {}, [
				E('p', {}, _('This removes %s and stops the usque service. The device registration itself remains on the Cloudflare side; registering again creates a new device.').format(dash(this.status && this.status.config_path))),
				E('div', { 'class': 'right', 'style': 'margin-top:1em' }, [
					E('button', { 'class': 'cbi-button', 'click': ui.hideModal }, _('Cancel')),
					' ',
					E('button', {
						'class': 'cbi-button cbi-button-negative important',
						'click': function() {
							ui.hideModal();
							L.resolveDefault(callDeleteConfig(), null).then(function(res) {
								if (!res || res.ok !== true)
									ui.addNotification(_('Delete failed'), E('p', {}, errText(res)), 'danger');
								else
									ui.addNotification(_('WARP configuration deleted'), E('p', {},
										_('The configuration file was removed and the service was stopped.')), 'success');
								return self.refresh();
							});
						}
					}, _('Delete'))
				])
			])
		]);
	},

	beginRegistration: function(name) {
		var self = this;

		this.registering = true;
		this.regT0 = Date.now();
		this.regText.textContent = _('Requesting a WARP registration from Cloudflare…');
		this.regOutput.textContent = '';
		this.regOutput.style.display = 'none';
		this.regBox.style.display = '';
		this.btnGenerate.disabled = true;

		return L.resolveDefault(callRegister(name, true), null).then(function(res) {
			if (!res || res.ok !== true) {
				self.registering = false;
				self.btnGenerate.disabled = false;
				self.regBox.style.display = 'none';
				ui.addNotification(_('Registration failed'), E('p', {}, errText(res)), 'danger');
				return;
			}

			/* the rpcd method returned immediately; poll registerStatus */
			self.regText.textContent = _('Registering with Cloudflare…');
			return self.refresh();
		});
	},

	updateRegister: function(res) {
		var elapsed;

		if (!this.registering)
			return;

		if (!res || res.ok !== true) {
			this.registering = false;
			this.btnGenerate.disabled = false;
			this.regBox.style.display = 'none';
			ui.addNotification(_('Registration failed'), E('p', {}, errText(res)), 'danger');
			return;
		}

		elapsed = res.elapsed > 0 ? res.elapsed : Math.round((Date.now() - (this.regT0 || Date.now())) / 1000);

		if (!res.done) {
			this.regText.textContent = _('Registering with Cloudflare… %s').format(fmtDuration(elapsed));
			if (res.output) {
				this.regOutput.style.display = '';
				this.regOutput.textContent = res.output;
			}
			return;
		}

		this.registering = false;
		this.btnGenerate.disabled = false;

		if (res.rc === 0) {
			this.regBox.style.display = 'none';
			ui.addNotification(_('WARP configuration created'), E('p', {},
				_('Enable the service and press Start to connect.')), 'success');
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
			_('See the output in the WARP account section.')), 'danger');
		return this.refresh();
	},

	/* ---------------------------------------------------------------------
	 * save / apply
	 * ------------------------------------------------------------------ */


	applyServiceState: function() {
		var self = this;
		var rawEnabled = uci.get('usque', 'main', 'enabled');
		var enabled = (rawEnabled != null) ? parseBool(rawEnabled) : null;

		/* ask the backend instead of trusting possibly stale polled state */
		return L.resolveDefault(callGetConfig(), null).then(function(cfg) {
			return self.applyServiceStateWithConfig(enabled, !!(cfg && cfg.ok === true && cfg.exists === true));
		})['catch'](function(err) {
			ui.addNotification(_('Service control failed'), E('p', {}, err.message || String(err)), 'danger');
		});
	},

	applyServiceStateWithConfig: function(enabled, haveConfig) {
		var self = this;
		var tasks = [];

		if (enabled === true)
			tasks.push(callSetInitAction('enable'));
		else if (enabled === false)
			tasks.push(callSetInitAction('disable'));

		if (enabled === false) {
			tasks.push(callSetInitAction('stop'));
		}
		else if (haveConfig) {
			tasks.push(callSetInitAction('restart'));
		}
		/* enabled but no config: do not start yet, the init script would
		 * register a device synchronously (blocking this call for up to a
		 * minute); the registration UI or the next boot handles that. */

		return tasks.reduce(function(chain, task) {
			return chain.then(function() {
				return L.resolveDefault(task, null).then(function(res) {
					if (!res || res.ok !== true)
						throw new Error(errText(res));
				});
			});
		}, Promise.resolve()).then(function() {
			if (enabled !== false && !haveConfig)
				ui.addNotification(_('Configuration saved'), E('p', {},
					_('No WARP configuration yet: use "Generate WARP configuration" first, or let the init script register a device on the next start (auto_register).')), 'warning');
			else if (enabled === false)
				ui.addNotification(_('Configuration saved'), E('p', {},
					_('usque was stopped and disabled (enabled=0).')), 'success');
			else
				ui.addNotification(_('Configuration saved'), E('p', {},
					_('usque was restarted with the new configuration.')), 'success');
			return self.refresh();
		}).catch(function(err) {
			ui.addNotification(_('Service control failed'), E('p', {}, err.message || String(err)), 'danger');
		});
	},

	handleSaveApply: function(ev) {
		var self = this;

		/* 1. stage the form values in the uci session (View.handleSave)
		 * 2. commit them (uci apply) so the init script sees them
		 * 3. enable/disable + restart the service
		 * 4. drop the "unsaved changes" indicator */
		return this.super('handleSave', [ ev ])
			.then(function() { return callUciApply(10, false); })
			.then(function() { return self.applyServiceState(); })
			.then(function() {
				/* everything is committed now, drop the stale change badge */
				if (ui.changes && typeof(ui.changes.setIndicator) === 'function')
					ui.changes.setIndicator(0);
			})
			.catch(function(err) {
				ui.addNotification(_('Save failed'), E('p', {}, err.message || String(err)), 'danger');
				return self.refresh();
			});
	}
});
