/* luci-app-online-upgrade v2 —— 原生 JS 前端（不依赖 LuCI 框架）。
 * RPC：通过 uhttpd-mod-ubus 以 JSON-RPC 调用 luci.online-upgrade 对象。
 * 会话令牌：使用 LuCI 管理会话 Cookie（调度器会话本身就是一个
 * ubus 会话），并以服务端注入的全局变量作为兜底。 */
(function () {
	'use strict';

	/* ---------------- 会话 ---------------- */

	function readCookie(name) {
		var m = document.cookie.match(new RegExp('(?:^|;\\s*)' + name + '=([^;]*)'));
		return m ? decodeURIComponent(m[1]) : '';
	}

	function luciSession() {
		try { return (window.L && window.L.env && window.L.env.sessionid) || ''; }
		catch (e) { return ''; }
	}

	var SID = window.__OU_SESSION__ || luciSession() ||
		readCookie('sysauth_https') || readCookie('sysauth_http') || readCookie('sysauth') || '';

	/* ---------------- ubus JSON-RPC 调用 ---------------- */

	var rpcId = 0;

	function ubus(method, params) {
		return fetch('/ubus/', {
			method: 'POST',
			credentials: 'same-origin',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				jsonrpc: '2.0', method: 'call',
				params: [SID, 'luci.online-upgrade', method, params || {}],
				id: ++rpcId
			})
		}).then(function (r) {
			if (r.status === 403 || r.status === 401)
				throw new Error('会话已过期,请刷新页面重新登录 LuCI');
			return r.json();
		}).then(function (res) {
			if (res.error) throw new Error('RPC error ' + res.error.code);
			var result = res.result;
			if (!Array.isArray(result)) throw new Error('invalid rpc reply');
			if (result[0] !== 0) throw new Error('ubus error ' + result[0] + (SID ? '' : '(未获取到会话)'));
			return result[1];
		});
	}

	/* ---------------- 工具函数 ---------------- */

	function $(id) { return document.getElementById(id); }

	function esc(s) {
		return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
			return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
		});
	}

	function fmtSize(n) {
		n = Number(n) || 0;
		if (n >= 1073741824) return (n / 1073741824).toFixed(1) + ' GB';
		if (n >= 1048576) return (n / 1048576).toFixed(1) + ' MB';
		if (n >= 1024) return (n / 1024).toFixed(1) + ' KB';
		return n + ' B';
	}

	function fmtTime(ts) {
		ts = Number(ts) || 0;
		if (!ts) return '—';
		var d = new Date(ts * 1000);
		function p(x) { return (x < 10 ? '0' : '') + x; }
		return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) +
			' ' + p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
	}

	function hint(id, text, cls) {
		var el = $(id);
		if (!el) return;
		el.textContent = text || '';
		el.className = 'ou-hint hint' + (cls ? ' ' + cls : '');
	}

	function setText(id, text) {
		var el = $(id);
		if (el) el.textContent = text;
	}

	var pollTimer = null;

	function stopPoll() {
		if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
	}

	function setBusy(btnId, busy, label) {
		var b = $(btnId);
		if (!b) return;
		b.disabled = !!busy;
		if (label !== undefined) {
			var txt = b.querySelector('.btn-label') || b.querySelector('span');
			if (txt) txt.textContent = label;
			else b.textContent = label;
		}
	}

	/* ---------------- 通用弹窗 ---------------- */

	var dialogResolve = null;
	var dialogPrevFocus = null;
	var dialogOnClose = null;
	var OU_DIALOG_ICONS = {
		primary: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><polyline points="9 11 12 14 17 9"/></svg>',
		danger: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>',
		warning: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>',
		success: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>'
	};

	function ouDialog(opts) {
		opts = opts || {};
		return new Promise(function (resolve) {
			var root = $('ou-dialog');
			if (!root) {
				// 极端降级：模板缺失时仍然保证操作可继续。
				resolve(window.confirm(opts.message || ''));
				return;
			}
			if (dialogResolve) {
				var previous = dialogResolve;
				dialogResolve = null;
				if (dialogOnClose) {
					try { dialogOnClose(); } catch (e) { /* ignore */ }
				}
				previous(false);
			}
			dialogResolve = resolve;
			dialogOnClose = opts.onClose || null;
			dialogPrevFocus = document.activeElement;
			var tone = opts.tone || 'primary';
			if (!OU_DIALOG_ICONS[tone]) tone = 'primary';
			var alertOnly = !!opts.alertOnly;

			root.className = 'ou-dialog tone-' + tone + (opts.wide ? ' is-wide' : '');
			root.style.display = 'flex';
			root.setAttribute('aria-hidden', 'false');
			setText('ou-dialog-title', opts.title || '请确认');
			setText('ou-dialog-eyebrow', opts.eyebrow || (alertOnly ? '提示' : '确认操作'));
			$('ou-dialog-icon').innerHTML = OU_DIALOG_ICONS[tone];

			var body = $('ou-dialog-body');
			var html = opts.html ? opts.html :
				('<p class="ou-dialog-message">' + esc(opts.message || '') + '</p>');
			if (!opts.html && opts.detail) {
				html += '<div class="ou-dialog-detail">' + esc(opts.detail) + '</div>';
			}
			body.innerHTML = html;
			body.scrollTop = 0;

			var cancel = $('ou-dialog-cancel');
			var confirm = $('ou-dialog-confirm');
			cancel.classList.toggle('is-hidden', alertOnly);
			cancel.textContent = opts.cancelText || '取消';
			confirm.textContent = opts.confirmText || (alertOnly ? '知道了' : '确定');
			confirm.className = 'btn ou-btn ' + (tone === 'danger' ? 'ou-btn-danger' :
				tone === 'warning' ? 'ou-btn-warning' : 'ou-btn-primary');

			document.body.classList.add('ou-modal-open');
			// 先强制提交初始 transform，再同步添加 is-open；
			// 不依赖 requestAnimationFrame（部分 WebView/无头环境不会触发）。
			void root.offsetWidth;
			root.classList.add('is-open');
			setTimeout(function () {
				if (!dialogResolve) return;
				(alertOnly ? confirm : cancel).focus();
			}, 60);
		});
	}

	function finishDialog(ok) {
		var root = $('ou-dialog');
		var resolve = dialogResolve;
		if (!root || !resolve) return;
		dialogResolve = null;
		var onClose = dialogOnClose;
		dialogOnClose = null;
		root.classList.remove('is-open');
		root.setAttribute('aria-hidden', 'true');
		document.body.classList.remove('ou-modal-open');
		if (dialogPrevFocus && dialogPrevFocus.focus) {
			try { dialogPrevFocus.focus(); } catch (e) { /* ignore */ }
		}
		dialogPrevFocus = null;
		setTimeout(function () {
			if (!dialogResolve) root.style.display = 'none';
		}, 240);
		resolve(ok === true);
		if (onClose) {
			try { onClose(); } catch (e) { /* ignore */ }
		}
	}

	function ouConfirm(opts) { return ouDialog(opts); }

	function ouAlert(opts) {
		opts = opts || {};
		var copy = {};
		for (var k in opts) {
			if (Object.prototype.hasOwnProperty.call(opts, k)) copy[k] = opts[k];
		}
		copy.alertOnly = true;
		return ouDialog(copy);
	}

	function bindDialog() {
		var root = $('ou-dialog');
		if (!root) return;
		$('ou-dialog-cancel').addEventListener('click', function () { finishDialog(false); });
		$('ou-dialog-confirm').addEventListener('click', function () { finishDialog(true); });
		$('ou-dialog-close').addEventListener('click', function () { finishDialog(false); });
		var backdrop = root.querySelector('[data-ou-dialog-dismiss]');
		if (backdrop) backdrop.addEventListener('click', function () { finishDialog(false); });
		document.addEventListener('keydown', function (e) {
			if (!dialogResolve) return;
			if (e.key === 'Escape') {
				e.preventDefault();
				finishDialog(false);
				return;
			}
			if (e.key !== 'Tab') return;
			var buttons = root.querySelectorAll('.ou-dialog-panel button:not([disabled])');
			var visible = [];
			for (var i = 0; i < buttons.length; i++) {
				if (buttons[i].offsetParent !== null) visible.push(buttons[i]);
			}
			if (!visible.length) return;
			var first = visible[0];
			var last = visible[visible.length - 1];
			if (e.shiftKey && document.activeElement === first) {
				e.preventDefault();
				last.focus();
			} else if (!e.shiftKey && document.activeElement === last) {
				e.preventDefault();
				first.focus();
			}
		});
	}

	/* ---------------- detached 运行状态的新旧判定 ---------------- */

	// rpcd 以 detached 方式启动 check/upgrade，点击后第一次轮询读到的往往还是
	// 上一次运行留下的终态（failed / check_done）。用「写入者 pid」判断新旧：
	// 新进程必然会写入不同的 pid，而浏览器与路由器的时钟可能不一致，不能用
	// 客户端时间戳比较。pid 缺失（旧版本写入的状态文件）时退化为时间戳比较。
	var pollBaseline = { pid: null, ts: 0 };
	var pollFilter = false;

	function snapshotStatus() {
		return ubus('get_status').then(function (st) {
			pollBaseline = {
				pid: (st && st.pid != null) ? st.pid : null,
				ts: Number((st && st.ts) || 0)
			};
		}).catch(function () {
			pollBaseline = { pid: null, ts: 0 };
		});
	}

	function isFreshRun(st) {
		if (!st) return false;
		if (st.pid != null && pollBaseline.pid != null)
			return st.pid !== pollBaseline.pid;
		return Number(st.ts || 0) > pollBaseline.ts;
	}

	/* ---------------- 状态卡片 ---------------- */

	var lastCheck = null;

	// 判定矩阵的每一档都给出用户可读文案，界面上既能看出命中哪条规则，
	// 也能看出是哪一步没通过（机型未匹配 / 架构未声明 / 架构冲突）。
	var ARCH_CHECK_TEXT = {
		ok: '架构一致',
		conflict: '架构冲突',
		undeclared: '资产未声明架构',
		unknown: '本机架构未知'
	};
	var MODEL_CHECK_TEXT = {
		exact: '机型匹配',
		partial: '机型部分匹配',
		rule: '命中配置规则',
		unmatched: '未匹配机型'
	};
	var RELATION_TEXT = {
		newer: '目标版本更高',
		same: '同版本',
		older: '目标版本更低',
		unknown: '版本无法比较'
	};
	// 版本判定的三种状态：可信可判定 / 弱依据可疑 / 确实无法比较。
	// 无法比较不等于阻断，只是把依据说清楚后按「升级到该发布版本」处理。
	var STATE_TEXT = {
		decidable: '版本可判定',
		suspect: '判定依据较弱',
		incomparable: '版本无法比较'
	};
	var STATE_DESC = {
		decidable: '本地与目标都取到了可信版本号，可直接比较版本方向',
		suspect: '本地版本来自构建时间、修订号或包版本等弱依据，方向仅供参考',
		incomparable: '取不到可比较的本地版本号，按「将升级到该发布版本」处理；机型、架构与哈希校验仍然生效'
	};
	var CONFIDENCE_TEXT = {
		strong: '可信',
		weak: '弱',
		none: '无'
	};
	var HASH_SOURCE_TEXT = {
		expected_sha256: '配置中的自定义校验值',
		release_asset: 'Release 校验清单',
		release_body: 'Release 说明正文',
		none: '未提供校验依据'
	};

	function compatLevel(ci) {
		if (!ci) return 'compatible';
		return ci.level || (ci.can_upgrade ? 'compatible' : 'incompatible');
	}

	// 同版本不构成「有更新」：既不能出现「有新版本」引导，也不能把普通升级摆成主操作，
	// 只保留显式点击的强制升级（同版本重装）。
	var FORCE_LABEL_SAME = '同版本重装（强制升级）';

	function sameVersionChecked() {
		return !!(lastCheck && lastCheck.version && lastCheck.version.relation === 'same');
	}

	function forceLabel() {
		return sameVersionChecked() ? FORCE_LABEL_SAME : '强制更新';
	}

	// .ou-container .ou-btn 带 display:inline-flex !important，inline 的 display:none 会被压过，
	// 所以按钮显隐统一走 is-hidden 类，同版本时才能真正把「立即升级」收起来。
	function showBtn(id, on) {
		var el = $(id);
		if (!el) return;
		if (on) el.classList.remove('is-hidden');
		else el.classList.add('is-hidden');
		el.style.display = on ? '' : 'none';
	}

	function tagChip(text, cls, title) {
		return '<span class="ou-tag-chip' + (cls ? ' ' + cls : '') + '"' +
			(title ? ' title="' + esc(title) + '"' : '') + '>' + esc(text) + '</span>';
	}

	function factRow(label, valueHtml, opts) {
		opts = opts || {};
		return '<div class="ou-fact' + (opts.wide ? ' ou-fact-wide' : '') + '">' +
			'<span class="ou-fact-label">' + esc(label) + '</span>' +
			'<span class="ou-fact-value' + (opts.mono ? ' ou-mono' : '') + '">' + valueHtml + '</span>' +
			'</div>';
	}

	function setEvidenceState(text, cls) {
		var el = $('evidence-state');
		if (!el) return;
		el.className = 'ou-evidence-state' + (cls ? ' ' + cls : '');
		el.textContent = text;
	}

	function setEvidenceExpanded(open) {
		var panel = $('check-evidence');
		var btn = $('evidence-toggle');
		var detail = $('evidence-detail');
		if (panel) panel.classList.toggle('is-open', !!open);
		if (btn) {
			btn.setAttribute('aria-expanded', open ? 'true' : 'false');
			var label = btn.querySelector('span');
			if (label) label.textContent = open ? '收起判定详情' : '展开判定详情';
		}
		if (detail) detail.open = !!open && !detail.classList.contains('is-hidden');
	}

	function hideEvidenceDetail() {
		var d = $('evidence-detail');
		if (d) d.classList.add('is-hidden');
		var l = $('evidence-release-link');
		if (l) l.classList.add('is-hidden');
	}

	function renderEvidenceLoading() {
		setEvidenceState('检测中', 'busy');
		setText('evidence-verdict', '正在连接 GitHub 并读取 Release 清单，请稍候…');
		if ($('evidence-chips')) $('evidence-chips').innerHTML = '';
		if ($('evidence-facts')) $('evidence-facts').innerHTML = '';
		setEvidenceExpanded(false);
		hideEvidenceDetail();
	}

	function renderEvidenceError(msg) {
		setEvidenceState('检测异常', 'err');
		setText('evidence-verdict', msg || '检测失败');
		if ($('evidence-chips')) $('evidence-chips').innerHTML = '';
		if ($('evidence-facts')) {
			$('evidence-facts').innerHTML = factRow('错误信息', esc(msg || '检测失败'), { wide: true });
		}
		setEvidenceExpanded(false);
		hideEvidenceDetail();
	}

	// 把检测结果拆成「结论 / 短标签 / 关键事实 / 可展开依据」四层。
	// 长值（规则正则、板型、哈希）进入事实网格并允许任意换行，
	// 不再挤在标题旁形成参差的标签流。
	function renderEvidence(ci, res, lv, hasUpdate, sameVersion) {
		var dev = res.device || {};
		var ver = res.version || {};
		var latest = res.latest || {};
		var checks = (ci && ci.checks) || {};
		var stateText = hasUpdate ? '发现新版本' : '已是最新';
		var stateCls = 'ok';
		if (lv === 'incompatible') { stateText = '固件不匹配'; stateCls = 'err'; }
		else if (lv === 'warning') { stateText = '需人工核对'; stateCls = 'warn'; }
		else if (lv === 'unknown') { stateText = '适配未知'; stateCls = 'warn'; }
		else if (sameVersion) { stateText = '已是最新（同版本）'; stateCls = 'ok'; }
		else if (ver.relation === 'older') { stateText = '目标版本更低'; stateCls = 'warn'; }
		else if (hasUpdate && ver.state && ver.state !== 'decidable') { stateText = '发现新版本（依据较弱）'; stateCls = 'warn'; }
		setEvidenceState(stateText, stateCls);

		setText('evidence-verdict', res.reason || '检测完成');

		var chips = [];
		if (ci) {
			var ccls = lv === 'compatible' ? 'ou-chip-ok'
				: (lv === 'warning' || lv === 'unknown') ? 'ou-chip-warn' : 'ou-chip-err';
			chips.push(tagChip(ci.badge || '适配评估', ccls, ci.reason || ''));
		}
		if (ver.relation) {
			var rcls = ver.relation === 'newer' ? 'ou-chip-ok'
				: ver.relation === 'older' ? 'ou-chip-err'
				: ver.relation === 'same' ? '' : 'ou-chip-warn';
			chips.push(tagChip(RELATION_TEXT[ver.relation] || ver.relation, rcls));
		}
		if (ver.state) {
			chips.push(tagChip(STATE_TEXT[ver.state] || ver.state,
				ver.state === 'decidable' ? 'ou-chip-ok' : 'ou-chip-warn', STATE_DESC[ver.state] || ''));
		}
		var chipsEl = $('evidence-chips');
		if (chipsEl) chipsEl.innerHTML = chips.join('');

		var facts = [];
		var model = (ci && ci.detected_model) || dev.model || '未知';
		var board = dev.board_name || dev.board_id || '—';
		var arch = dev.arch || (ci && ci.detected_arch) || '未知';
		var pkgArch = (dev.pkg_arch && dev.pkg_arch !== arch) ? '（' + dev.pkg_arch + '）' : '';
		facts.push(factRow('本机身份',
			'<span class="ou-fact-strong">' + esc(model) + '</span> · board ' + esc(board) +
			' · ' + esc(arch) + esc(pkgArch), { wide: true }));

		facts.push(factRow('机型通道',
			'<span class="ou-fact-strong">' + esc(MODEL_CHECK_TEXT[checks.model] || '未比对') + '</span>' +
			(ci && ci.detected_model ? ' · 识别为 ' + esc(ci.detected_model) : '')));

		facts.push(factRow('架构通道',
			'<span class="ou-fact-strong">' + esc(ARCH_CHECK_TEXT[checks.arch] || '未比对') + '</span>' +
			' · 资产 ' + esc((ci && ci.asset_arch) || '未声明') +
			' → 本机 ' + esc((ci && ci.device_arch) || '未知')));

		var target = ver.target || latest.version || '未知';
		var local = ver.local || '无法取到';
		facts.push(factRow('版本通道',
			'目标 <span class="ou-fact-strong">v' + esc(target) + '</span>' +
			' · ' + esc(RELATION_TEXT[ver.relation] || '无法比较') +
			' · 本地 ' + esc(local)));

		var conf = CONFIDENCE_TEXT[ver.local_confidence] || ver.local_confidence || '未知';
		facts.push(factRow('本地版本依据',
			esc(ver.local_source_text || ver.local_source || '来源未知') +
			' · 可信度 <span class="ou-fact-strong">' + esc(conf) + '</span>' +
			(ver.state ? ' · ' + esc(STATE_TEXT[ver.state] || ver.state) : '')));

		var hashVal;
		if (latest.sha256) {
			hashVal = esc((latest.sha256_source ? (HASH_SOURCE_TEXT[latest.sha256_source] || latest.sha256_source) : '来源未知')) +
				' · <span class="ou-mono">' + esc(latest.sha256.substring(0, 16)) + '…</span>';
		} else {
			hashVal = '<span class="ou-fact-muted">无上游校验依据，下载后仅记录本地 SHA256</span>';
		}
		facts.push(factRow('校验依据', hashVal));

		facts.push(factRow('发布信息',
			'标签 ' + esc(res.resolved_tag || latest.version || '—') +
			' · ' + esc(latest.updated_local || '—') +
			' · ' + esc(latest.size_human || '—')));

		if (ci && ci.matched_rule) {
			facts.push(factRow('命中配置规则', esc(ci.matched_rule), { wide: true, mono: true }));
		}
		var factsEl = $('evidence-facts');
		if (factsEl) factsEl.innerHTML = facts.join('');

		var basis = (ci && ci.basis) || [];
		var detail = $('evidence-detail');
		var list = $('evidence-basis');
		if (list) {
			list.innerHTML = basis.map(function (b) { return '<li>' + esc(b) + '</li>'; }).join('');
		}
		if ($('evidence-basis-count')) {
			$('evidence-basis-count').textContent = basis.length + ' 条';
		}
		if (detail) detail.classList.toggle('is-hidden', basis.length === 0);

		var link = $('evidence-release-link');
		if (link) {
			if (res.release_url) {
				link.href = res.release_url;
				link.classList.remove('is-hidden');
			} else {
				link.classList.add('is-hidden');
			}
		}
		setEvidenceExpanded(false);
	}

	function renderCheckResult(res) {
		lastCheck = res;
		var badge = $('status-badge');
		var box = $('check-result-box');
		if (!res || res.ok !== true) {
			var errText = (res && res.error) ? res.error : '检测失败';
			hint('check-result', errText, 'err');
			if (box) box.className = 'ou-notice-box err';
			if (badge) {
				badge.className = 'ou-status-pill err';
				badge.innerHTML = '<span class="ou-dot err"></span> 检测异常';
			}
			renderEvidenceError(errText);
			showBtn('btn-force', true);
			showBtn('btn-upgrade', false);
			return;
		}

		var latest = res.latest || {};
		setText('latest-name', latest.name);
		setText('new-ver', latest.version || res.resolved_tag || '未解析版本');
		setText('latest-size', latest.size_human);
		setText('latest-time', latest.updated_local);

		var hashStatus = $('latest-hash-status');
		if (hashStatus) {
			if (latest.sha256) {
				var shortH = latest.sha256.substring(0, 8) + '...' +
					latest.sha256.substring(latest.sha256.length - 8);
					hashStatus.textContent = 'SHA256: ' + shortH;
					hashStatus.title = '完整 SHA256: ' + latest.sha256 + ' (来源: ' +
						(HASH_SOURCE_TEXT[latest.sha256_source] || latest.sha256_source || '未知') + ')';
			} else {
				// 上游既没有校验单也没有正文哈希时，下载后只能记录本地哈希，
				// 不能对外宣称「已校验」，否则等于给出虚假的安全承诺。
				hashStatus.textContent = '无校验依据，仅记录本地哈希';
				hashStatus.title = '上游未提供校验清单或期望哈希，下载后只记录本地 SHA256，无法比对校验';
			}
		}

		var ci = res.compatibility || null;
		var lv = compatLevel(ci);
		var compatBadgeEl = $('latest-compat-badge');
		if (compatBadgeEl) {
			if (ci) {
				var cls = lv === 'compatible' ? 'ok'
					: (lv === 'warning' || lv === 'unknown') ? 'warn' : 'err';
				compatBadgeEl.innerHTML = '<span class="ou-compat-chip ' + cls + '">' +
					esc(ci.badge || '适配评估') + '</span>';
				compatBadgeEl.title = ci.reason || '';
			} else {
				compatBadgeEl.textContent = '未经适配判定';
			}
		}

		var hasUpdate = res.has_update === true;
		var ver = res.version || null;
		var sameVersion = !!(ver && ver.relation === 'same');
		renderEvidence(ci, res, lv, hasUpdate, sameVersion);

		var msg;
		var lvl;

		if (lv === 'incompatible') {
			msg = '固件与本机不匹配，已阻止升级: ' + (ci.reason || '');
			lvl = 'err';
			if (badge) {
				badge.className = 'ou-status-pill err';
				badge.innerHTML = '<span class="ou-dot err"></span> 固件不匹配';
			}
		} else if (lv === 'warning') {
			msg = (sameVersion
				? '需人工核对后才可重装本机固件(同版本重装): '
				: '需人工核对后才可升级: ') + (ci.reason || '') +
				(sameVersion
					? '。确认无误后可用「强制升级」重装。'
					: '。确认无误后可用「强制更新」继续。');
			lvl = 'warn';
			if (badge) {
				badge.className = 'ou-status-pill update';
				badge.innerHTML = '<span class="ou-dot update"></span> 需人工核对';
			}
		} else if (lv === 'unknown') {
			msg = (sameVersion
				? '无法确认该固件是否适配本机，已是最新同版本: '
				: '无法确认是否适配本机，未自动判定为可升级: ') + (ci.reason || '') +
				(sameVersion
					? '。如确认该文件适用于本机，可用「强制升级」重装。'
					: '。如确认该文件适用于本机，可用「强制更新」并显式确认。');
			lvl = 'warn';
			if (badge) {
				badge.className = 'ou-status-pill update';
				badge.innerHTML = '<span class="ou-dot update"></span> 无法确认适配';
			}
		} else if (sameVersion) {
			// 同版本：不出现「有新版本 / 发现更新」引导，也不把普通升级按钮当成主操作，
			// 只保留显式点击的强制升级（同版本重装）。详细依据放在证据面板，
			// 这里只给下一步动作，避免同一句话重复两遍。
			msg = '无需升级：当前固件与 Release 同版本。如需同版本重装，请点击「强制升级」。';
			if (ver.state === 'suspect') msg += ' 本地版本依据较弱，仅供参考。';
			lvl = 'ok';
			if (badge) {
				badge.className = 'ou-status-pill ok';
				badge.innerHTML = '<span class="ou-dot ok"></span> 已是最新（同版本）';
			}
		} else if (hasUpdate) {
			var v = ver || {};
			if (v.state === 'incomparable') {
				// 取不到本地版本号不等于「不能升级」：明确说明依据后放行，
				// 但把仍然生效的安全闸一起讲清楚，避免用户以为跳过了校验。
				msg = (res.reason || ('将升级到 ' + (v.target || '该发布版本'))) +
					'。这不会跳过任何安全检查：机型匹配、架构匹配与 SHA-256 校验照旧生效。';
				lvl = 'warn';
				if (badge) {
					badge.className = 'ou-status-pill update';
					badge.innerHTML = '<span class="ou-dot update"></span> 有新版本（版本无法比较）';
				}
			} else if (v.state === 'suspect') {
				msg = (res.reason || '发现新版本') +
					'。本地版本依据较弱，建议核对后再升级。';
				lvl = 'warn';
				if (badge) {
					badge.className = 'ou-status-pill update';
					badge.innerHTML = '<span class="ou-dot update"></span> 有新版本（依据较弱）';
				}
			} else {
				msg = '发现新版本 v' + (v.target || latest.version || '') + '，可点击「立即升级」开始更新。';
				lvl = 'ok';
				if (badge) {
					badge.className = 'ou-status-pill update';
					badge.innerHTML = '<span class="ou-dot update"></span> 有新版本';
				}
			}
		} else {
			// 目标版本低于当前时 has_update=false，但这不是「已是最新」：
			// 必须明确告诉用户「不执行降级」，并保留强制升级入口的解释。
			if (ver && ver.relation === 'older') {
				msg = res.reason || '目标版本低于当前版本，不执行降级；如确需降级请使用「强制升级」。';
				lvl = 'warn';
				if (badge) {
					badge.className = 'ou-status-pill update';
					badge.innerHTML = '<span class="ou-dot update"></span> 不执行降级';
				}
			} else {
				msg = res.reason || '已是最新版本';
				lvl = 'ok';
				if (badge) {
					badge.className = 'ou-status-pill ok';
					badge.innerHTML = '<span class="ou-dot ok"></span> 已是最新';
				}
			}
		}

		hint('check-result', msg, lvl);
		if (box) box.className = 'ou-notice-box' + (lvl === 'ok' ? ' ok' : (lvl === 'err' ? ' err' : ''));
		showBtn('btn-upgrade', lv === 'compatible' && hasUpdate && !sameVersion);
		if ($('btn-force')) {
			showBtn('btn-force', true);
			$('btn-force').title = sameVersion
				? '当前固件与 Release 同版本：这是同版本重装，不是升级，仅在你需要重刷固件时手动使用'
				: (ver && ver.relation === 'older'
					? '目标版本低于当前固件，强制模式将执行降级刷写'
					: (lv === 'compatible'
						? '即使已是最新版本也重新下载刷写'
						: '跳过适配判定与版本方向检查，强制下载并刷写'));
			setBusy('btn-force', false, forceLabel());
		}
	}

	function runCheck() {
		setBusy('btn-check', true, '检查中...');
		setBusy('btn-upgrade', true);
		setBusy('btn-force', true);
		hint('check-result', '正在连接云端检查固件更新...');
		renderEvidenceLoading();
		var box = $('check-result-box');
		if (box) box.className = 'ou-notice-box';
		var badge = $('status-badge');
		if (badge) {
			badge.className = 'ou-status-pill update';
			badge.innerHTML = '<span class="ou-dot update"></span> 检测中';
		}
		stopPoll();
		var skipped = 0;

		function finishCheck() {
			stopPoll();
			refreshLog();
			setBusy('btn-check', false, '检查更新');
			setBusy('btn-upgrade', false, '立即升级');
			setBusy('btn-force', false, forceLabel());
		}

		snapshotStatus().then(function () {
			pollFilter = true;
			return ubus('check');
		}).then(function (r) {
			// ubus 在 result[0]==0 时直接 resolve 业务 payload，撞锁的
			// {ok:false,error} 不会 reject；不判 ok 就会把「没启动」当成「已启动」，
			// 白等 20 秒后只给一句泛化提示。
			if (r && r.ok === false) {
				hint('check-result', r.error || '检测未能启动', 'err');
				renderEvidenceError(r.error || '检测未能启动');
				if (box) box.className = 'ou-notice-box err';
				finishCheck();
				return;
			}
			function checkTick() {
				if (skipped > 20) {
					hint('check-result', '未读取到本次检测的状态，可能有另一个操作正在进行，详见运行日志', 'err');
					finishCheck();
					return;
				}
				ubus('get_status').then(function (st) {
					if (pollFilter) {
						if (!isFreshRun(st)) { skipped++; return; }
						pollFilter = false;
					}
					if (st.stage === 'check_done' && st.result) {
						renderCheckResult(st.result);
						finishCheck();
					} else if (st.stage === 'check_failed' || st.stage === 'failed') {
						renderCheckResult({ ok: false, error: st.error || st.msg || '检测失败' });
						finishCheck();
					}
				}).catch(function () { /* transient rpc errors during poll: ignore */ });
			}
			checkTick();
			pollTimer = setInterval(checkTick, 1000);
		}).catch(function (e) {
			hint('check-result', '检测请求失败: ' + e.message, 'err');
			renderEvidenceError('检测请求失败: ' + e.message);
			if (box) box.className = 'ou-notice-box err';
			finishCheck();
		});
	}

	/* ---------------- 升级流程 ---------------- */

	function showRebootOverlay() {
		stopPoll();
		var ov = $('reboot-overlay');
		if (!ov || ov.style.display === 'flex') return;
		ov.style.display = 'flex';
		var seconds = 120;
		var cd = $('reboot-countdown');
		cd.textContent = seconds;
		var timer = setInterval(function () {
			seconds--;
			if (cd) cd.textContent = Math.max(seconds, 0);
		}, 1000);

		// 轮询直到路由器重新响应（LAN IP 可能变化，因此同时尝试主机名与常见网关地址）
		var targets = [location.origin, 'http://immortalwrt.lan', 'http://192.168.1.1'];
		var t = 0;
		var ping = setInterval(function () {
			fetch(targets[t % targets.length] + '/cgi-bin/luci/', {
				method: 'HEAD', cache: 'no-store',
				signal: (function () { var c = new AbortController(); setTimeout(function () { c.abort(); }, 3000); return c.signal; })()
			}).then(function () {
				clearInterval(ping);
				clearInterval(timer);
				location.href = targets[t % targets.length] + '/cgi-bin/luci/admin/system/online_upgrade';
			}).catch(function () { t++; });
		}, 3000);
	}

	var STAGE_TEXT = {
		checking: '正在检查固件更新...',
		check_done: '检测完成',
		check_failed: '检测失败',
		downloading: '正在下载固件...',
		paused: '下载已暂停，点击「继续」恢复',
		downloaded: '固件下载完成',
		saving: '记录固件版本...',
		backing_up: '正在创建配置备份...',
		backup_done: '备份完成',
		flashing: '正在刷写固件,配置将自动恢复!',
		restore_done: '配置恢复完成',
		failed: '操作失败'
	};

	function updatePipeline(stage) {
		var sEnv = $('step-env'), sDl = $('step-dl'), sBackup = $('step-backup'), sFlash = $('step-flash');
		if (!sEnv || !sDl || !sBackup || !sFlash) return;
		[sEnv, sDl, sBackup, sFlash].forEach(function (el) { el.className = 'ou-step-node'; });
		if (stage === 'checking' || stage === 'check_done') {
			sEnv.className = 'ou-step-node active';
		} else if (stage === 'downloading') {
			sEnv.className = 'ou-step-node done';
			sDl.className = 'ou-step-node active';
		} else if (stage === 'downloaded' || stage === 'saving' || stage === 'backing_up' || stage === 'backup_done') {
			sEnv.className = 'ou-step-node done';
			sDl.className = 'ou-step-node done';
			sBackup.className = 'ou-step-node active';
		} else if (stage === 'flashing' || stage === 'restore_done') {
			sEnv.className = 'ou-step-node done';
			sDl.className = 'ou-step-node done';
			sBackup.className = 'ou-step-node done';
			sFlash.className = 'ou-step-node active';
		}
	}

	function renderProgress(st) {
		if (!$('card-progress')) return;
		$('card-progress').style.display = '';
		$('progress-bar').style.width = (st.pct || 0) + '%';
		$('progress-pct').textContent = (st.pct || 0) + '%';
		$('progress-text').textContent = (st.error ? (' ' + st.error) : (st.msg || STAGE_TEXT[st.stage] || st.stage));
		updatePipeline(st.stage);
	}

	function pollUpgrade() {
		stopPoll();
		var skipped = 0;
		var ticks = 0;
		function tick() {
			Promise.all([ubus('get_status'), ubus('get_log')]).then(function (rs) {
				var st = rs[0], lg = rs[1];
				if (pollFilter) {
					if (!isFreshRun(st)) {
						if (++skipped > 20) {
							stopPoll();
							hint('check-result', '未读取到本次任务的状态，可能有另一个操作正在进行，详见运行日志', 'err');
						}
						return;
					}
					pollFilter = false;
				}
				if (lg && lg.log) {
					var lv = $('log-view');
					var text = lg.log;
					if (text.length > 2097152) text = text.substring(text.length - 2097152);
					lv.textContent = text;
					lv.scrollTop = lv.scrollHeight;
				}
				renderProgress(st);
				ticks++;
				if (ticks % 3 === 0) refreshTasks();
				if (st.stage === 'flashing') {
					showRebootOverlay();
				} else if (st.stage === 'failed') {
					stopPoll();
					setBusy('btn-check', false, '检查更新');
					showBtn('btn-force', true);
					$('card-progress').style.display = 'none';
					hint('check-result', '操作失败: ' + (st.error || '未知错误'), 'err');
					// 把停止原因（慢速证据、用户取消等）同步显示在任务卡片中，
					// 并刷新最终的历史任务状态。
					if (st.error) hint('task-hint', ' ' + st.error, 'err');
					refreshTasks();
				}
			}).catch(function () { /* keep polling; device may reboot mid-poll */ });
		}
		tick();
		pollTimer = setInterval(tick, 1000);
	}

	function startUpgrade(isForce) {
		var ci = lastCheck && lastCheck.compatibility;
		var lv = compatLevel(ci);
		var same = sameVersionChecked();
		if (!isForce && lv !== 'compatible') {
			hint('check-result', '当前目标固件未通过适配判定，需用「强制更新」并显式确认后才能继续', 'warn');
			return;
		}
		if (!isForce && same) {
			// 同版本不提供普通升级入口，重装只能显式走强制升级
			hint('check-result', '当前固件与 Release 同版本，普通升级不适用；如需重装请使用「强制升级」', 'warn');
			return;
		}
		var title = isForce
			? (same ? '同版本重装固件' : '强制更新固件')
			: '开始在线固件升级';
		var message = isForce
			? (same
				? '当前固件与 Release 为同一版本，这是重装而不是升级。\n强制模式会跳过适配判定与版本方向检查。\n系统仍会备份配置、下载并校验固件后再刷写。\n请勿断电！'
				: '强制模式会跳过适配判定与版本方向检查（目标固件可能无法确认是否适配本机，或版本低于当前）。\n系统仍会备份配置、下载并校验固件后再刷写。\n请勿断电！')
			: '系统将自动：备份配置 → 多线程下载固件 → 校验 → 刷写（自动恢复配置） → 重启。\n请勿断电！';
		ouConfirm({
			tone: isForce ? 'danger' : 'primary',
			eyebrow: isForce ? '强制刷写' : '在线升级',
			title: title,
			message: message,
			confirmText: isForce ? (same ? '确认重装' : '继续强制升级') : '开始升级'
		}).then(function (ok) {
			if (ok) doStartUpgrade(isForce);
		});
	}

	function doStartUpgrade(isForce) {
		var same = sameVersionChecked();
		setBusy('btn-upgrade', true, '启动中...');
		setBusy('btn-force', true, '启动中...');
		stopPoll();
		snapshotStatus().then(function () {
			pollFilter = true;
			return ubus('start_upgrade', { force: isForce === true });
		}).then(function (r) {
			// 同上：撞锁时后端返回 {ok:false,error}，ubus 不会 reject。若按「已启动」
			// 继续，两个按钮会被隐藏并进入对别的任务的无效轮询，页面卡死只能刷新。
			if (r && r.ok === false) {
				// 撞锁失败恢复按钮时也要维持「同版本不摆普通升级入口」的口径
				showBtn('btn-upgrade', !same);
				showBtn('btn-force', true);
				setBusy('btn-upgrade', false, '立即升级');
				setBusy('btn-force', false, forceLabel());
				$('card-progress').style.display = 'none';
				hint('check-result', r.error || '升级未能启动', 'err');
				return;
			}
			showBtn('btn-upgrade', false);
			showBtn('btn-force', false);
			setBusy('btn-upgrade', false, '立即升级');
			setBusy('btn-force', false, forceLabel());
			$('card-progress').style.display = '';
			pollUpgrade();
		}).catch(function (e) {
			setBusy('btn-upgrade', false, '立即升级');
			setBusy('btn-force', false, forceLabel());
			hint('check-result', '升级请求失败: ' + e.message, 'err');
		});
	}

	/* ---------------- 下载任务 ---------------- */

	var taskTimer = null;
	var taskActive = null;
	var taskDetailTimer = null;

	function stopTaskDetail() {
		if (taskDetailTimer) {
			clearInterval(taskDetailTimer);
			taskDetailTimer = null;
		}
	}

	function taskDetailHtml(t) {
		var total = Number(t.chunks_total) || 0;
		var states = t.chunk_states || '';
		var pct = t.total > 0 ? Math.min(100, Math.round(t.done / t.total * 100)) : 0;
		var cells = '';
		var limit = Math.min(total, 600);
		for (var i = 0; i < limit; i++) {
			var s = states.charAt(i);
			var cls = s === 'D' ? 'done' : s === 'R' ? 'running' : 'pending';
			cells += '<span class="ou-chunk-cell ' + cls + '" title="分块 ' + (i + 1) + ' · ' +
				(cls === 'done' ? '完成' : cls === 'running' ? '传输中' : '等待') + '"></span>';
		}
		var chunkBlock = total > 0
			? '<div class="ou-chunk-panel">' +
				'<div class="ou-chunk-head"><span>分块状态（已显示 ' + limit + ' / ' + total + '）</span>' +
				'<span class="ou-chunk-legend"><i class="done"></i>完成 <i class="running"></i>传输中 <i class="pending"></i>等待</span></div>' +
				'<div class="ou-chunk-grid">' + cells + '</div></div>'
			: '<div class="ou-dialog-detail">该任务没有分块明细（旧任务或单流下载）。</div>';
		return '<div class="ou-task-detail" id="ou-task-detail-body">' +
			'<div class="ou-task-detail-grid">' +
				'<div class="ou-task-detail-item"><span>类型</span><strong>' + esc(t.kind === 'plugin' ? '插件更新' : '固件下载') + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>状态</span><strong>' + esc(taskDisplayState(t)) + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>进度</span><strong>' + pct + '% · ' + esc(fmtSize(t.done)) + ' / ' + esc(fmtSize(t.total)) + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>实时速度</span><strong>' + esc(fmtSpeed(t.speed_bps)) + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>线程</span><strong>' + (t.segments || 0) + ' / ' + (t.workers || 0) + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>分块</span><strong>' + (t.chunks_done || 0) + ' / ' + (t.chunks_total || 0) + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>分块大小</span><strong>' + esc(t.chunk_size ? fmtSize(t.chunk_size) : '—') + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>传输中分块</span><strong>' + (t.chunks_running || 0) + '</strong></div>' +
				'<div class="ou-task-detail-item"><span>原因 / 阶段</span><strong>' + esc(t.reason || '—') + '</strong></div>' +
			'</div>' + chunkBlock + '</div>';
	}

	function showTaskDetail(id) {
		stopTaskDetail();
		var render = function (t) {
			var body = $('ou-task-detail-body');
			if (!body) return;
			if (!t) {
				body.innerHTML = '<div class="ou-dialog-detail">任务已被清理或删除。</div>';
				return;
			}
			body.outerHTML = taskDetailHtml(t);
		};
		ouDialog({
			tone: 'primary',
			eyebrow: '下载任务',
			title: '任务详情',
			html: '<div class="ou-task-detail" id="ou-task-detail-body"><div class="ou-dialog-detail">正在读取任务...</div></div>',
			alertOnly: true,
			confirmText: '关闭',
			wide: true,
			onClose: stopTaskDetail
		});
		var refresh = function () {
			ubus('list_tasks').then(function (r) {
				var list = (r && r.tasks) || [];
				var task = null;
				for (var i = 0; i < list.length; i++) {
					if (list[i].id === id) { task = list[i]; break; }
				}
				render(task);
			}).catch(function () { /* keep last snapshot */ });
		};
		refresh();
		taskDetailTimer = setInterval(refresh, 2000);
	}

	function taskStateLabel(s) {
		var m = {
			running: '下载中', paused: '已暂停', done: '已完成',
			failed: '失败', aborted: '已停止'
		};
		return m[s] || s || '未知';
	}

	// 插件任务在下载完成后还要校验与安装；用 reason 展示当前阶段，
	// 避免任务表一直显示“下载中”。
	function taskDisplayState(t) {
		if (t && t.kind === 'plugin' && t.state === 'running') {
			var reason = t.reason || '';
			// 注意“安装包”是下载阶段的文案，不能因此误判为安装中；
			// 只有“正在安装/安装插件包”才进入安装阶段。
			if (reason.indexOf('正在安装') >= 0 || reason.indexOf('安装插件包') >= 0) return '安装中';
			if (reason.indexOf('校验') >= 0) return '校验中';
			return '下载中';
		}
		return taskStateLabel(t && t.state);
	}

	function fmtSpeed(bps) {
		var n = Number(bps || 0);
		if (n <= 0) return '—';
		if (n >= 1048576) return (n / 1048576).toFixed(1) + ' MB/s';
		return (n / 1024).toFixed(1) + ' KB/s';
	}

	function syncTaskButtons(active, anyRunning) {
		var busy = !!active;
		// 插件更新任务也占用后端操作锁，因此任何任务运行期间都不应
		// 再启动新的固件下载；暂停/继续/取消仍只针对固件任务。
		setBusy('btn-start-download', busy || !!anyRunning, '开始下载');
		setBusy('btn-pause-download', active !== 'running', '暂停');
		setBusy('btn-resume-download', active !== 'paused', '继续');
		setBusy('btn-cancel-download', !busy, '取消');
	}

	function renderTasks(list) {
		var tbody = $('task-table').querySelector('tbody');
		tbody.innerHTML = '';
		var active = null;
		var anyRunning = false;
		(list || []).forEach(function (t) {
			var isPlugin = t.kind === 'plugin';
			var running = t.state === 'running' || t.state === 'paused';
			if (running) {
				anyRunning = true;
				// 固件下载控制按钮只跟随固件任务；插件任务由更新流程自动管理。
				if (!isPlugin) active = t.state;
			}
			var pct = t.total > 0 ? Math.min(100, Math.round(t.done / t.total * 100)) : 0;
			var progress = (running)
				? (pct + '% · ' + fmtSize(t.done) + ' / ' + fmtSize(t.total))
				: (pct + '% · ' + fmtSize(t.done));
			var kindChip = isPlugin
				? '<span class="ou-task-kind plugin">插件更新</span>'
				: '';
			var meta = (t.chunks_total > 0)
				? ('线程 ' + (t.segments || 0) + '/' + (t.workers || 0) +
					' · 分块 ' + (t.chunks_done || 0) + '/' + t.chunks_total)
				: '';
			var infoBtn = '<button type="button" class="btn ou-btn ou-btn-default ou-task-info" data-id="' +
				esc(t.id) + '"><span>详情</span></button>';
			var action = infoBtn + (isPlugin && running
				? '<span class="ou-time-text">自动更新中</span>'
				: '<button type="button" class="btn ou-btn ou-btn-danger ou-task-del" data-id="' + esc(t.id) + '"><span>删除</span></button>');
			var tr = document.createElement('tr');
				tr.innerHTML = '<td><div class="ou-file-cell"><span class="ou-file-text">' + esc(t.name) + '</span>' + kindChip + '</div></td>' +
				'<td data-label="状态"><span class="ou-badge-size">' + esc(taskDisplayState(t)) + ' · ' + esc(progress) + '</span>' +
				(meta ? '<span class="ou-task-meta">' + esc(meta) + '</span>' : '') + '</td>' +
				'<td data-label="速度"><span class="ou-time-text">' + esc(fmtSpeed(t.speed_bps)) + '</span></td>' +
				'<td data-label="原因"><span class="ou-time-text">' + esc(t.reason || '—') + '</span></td>' +
				'<td data-label="操作" style="text-align:right">' + action + '</td>';
			var del = tr.querySelector('.ou-task-del');
			if (del) del.addEventListener('click', function () { deleteTask(t.id, t.state); });
			var info = tr.querySelector('.ou-task-info');
			if (info) info.addEventListener('click', function () { showTaskDetail(t.id); });
			tbody.appendChild(tr);
		});
		if (!list || !list.length) {
			tbody.innerHTML = '<tr><td colspan="5" class="ou-empty">暂无下载任务</td></tr>';
		}
		taskActive = active;
		syncTaskButtons(active, anyRunning);
		// 任意运行中的任务都需要刷新列表（插件任务没有暂停/取消按钮，
		// 但必须持续展示下载/安装阶段和最终状态）。
		if (anyRunning && !taskTimer) {
			taskTimer = setInterval(refreshTasks, 3000);
		} else if (!anyRunning && taskTimer) {
			clearInterval(taskTimer);
			taskTimer = null;
		}
	}

	function refreshTasks() {
		return ubus('list_tasks').then(function (r) {
			renderTasks(r && r.tasks ? r.tasks : []);
		}).catch(function (e) {
			hint('task-hint', ' 任务列表读取失败: ' + e.message, 'err');
		});
	}

	function taskCommand(method, label) {
		return ubus(method).then(function (r) {
			if (r && r.ok === false) hint('task-hint', ' ' + (r.error || (label + '失败')), 'err');
			else hint('task-hint', ' ' + label + '已生效', 'ok');
			return refreshTasks();
		}).catch(function (e) {
			hint('task-hint', ' ' + label + '请求失败: ' + e.message, 'err');
		});
	}

	function startDownload() {
		setBusy('btn-start-download', true, '启动中...');
		snapshotStatus().then(function () {
			pollFilter = true;
			return ubus('start_download');
		}).then(function (r) {
			if (r && r.ok === false) {
				hint('task-hint', ' ' + (r.error || '下载未能启动'), 'err');
				return refreshTasks();
			}
			hint('task-hint', ' 下载任务已启动', 'ok');
			$('card-progress').style.display = '';
			pollUpgrade();
			return refreshTasks();
		}).catch(function (e) {
			hint('task-hint', ' 启动下载失败: ' + e.message, 'err');
			return refreshTasks();
		});
	}

	function deleteTask(id, state) {
		var busy = (state === 'running' || state === 'paused');
		if (busy) {
			ouAlert({
				tone: 'warning',
				eyebrow: '任务管理',
				title: '任务仍在进行中',
				message: '请先点击「取消」停止任务，再删除历史记录。',
				detail: id
			});
			return;
		}
		ouConfirm({
			tone: 'danger',
			eyebrow: '任务管理',
			title: '删除下载任务',
			message: '删除后不可恢复；若已无「已完成」记录，会一并清理临时固件文件。',
			detail: id,
			confirmText: '删除任务'
		}).then(function (ok) {
			if (!ok) return;
			ubus('delete_task', { id: id }).then(function (r) {
				if (r && r.ok) {
					hint('task-hint', ' 已删除任务 ' + id +
						(r.files_removed ? '（清理 ' + r.files_removed + ' 个临时文件）' : ''), 'ok');
				} else {
					hint('task-hint', ' ' + (r && r.error ? r.error : '删除失败'), 'err');
				}
				return refreshTasks();
			}).catch(function (e) {
				hint('task-hint', ' 删除失败: ' + e.message, 'err');
			});
		});
	}

	/* ---------------- 备份与恢复 ---------------- */

	function refreshBackups() {
		ubus('list_backups').then(function (r) {
			var tbody = $('backup-table').querySelector('tbody');
			tbody.innerHTML = '';
			if (!r.backups || !r.backups.length) {
				tbody.innerHTML = '<tr><td colspan="4" class="ou-empty">' +
					'<div class="ou-empty-box">' +
					'<svg class="ou-empty-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M21 8v13H3V8"/><path d="M1 3h22v5H1z"/><path d="M10 12h4"/></svg>' +
					'<div class="ou-empty-title">暂无历史配置备份</div>' +
					'<div class="ou-empty-sub">升级前后系统将自动创建快照，您也可以点击上方「创建备份」快速保存</div>' +
					'</div></td></tr>';
				hint('backup-hint', '');
				return;
			}
			r.backups.forEach(function (b) {
				var tr = document.createElement('tr');
				tr.innerHTML = '<td><div class="ou-file-cell">' +
					'<svg class="ou-file-svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><polyline points="13 2 13 9 20 9"/></svg>' +
					'<span class="ou-file-text">' + esc(b.name) + '</span></div></td>' +
					'<td><span class="ou-badge-size">' + fmtSize(b.size) + '</span></td>' +
					'<td><span class="ou-time-text">' + fmtTime(b.mtime) + '</span></td>' +
					'<td style="text-align:right"><button type="button" class="btn ou-btn ou-btn-danger ou-del-btn" data-path="' + esc(b.path) + '">' +
					'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="ou-btn-svg"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>' +
					'<span>删除</span></button></td>';
				tr.querySelector('.ou-del-btn').addEventListener('click', function () { deleteBackup(b.path); });
				tbody.appendChild(tr);
			});
			hint('backup-hint', '最新备份: ' + r.backups[0].path, 'ok');
		}).catch(function (e) {
			hint('backup-hint', ' ' + e.message, 'err');
		});
	}

	function runBackup() {
		setBusy('btn-backup', true, '备份中...');
		ubus('backup').then(function (r) {
			if (r && r.ok) hint('backup-hint', ' 备份成功: ' + r.path + ' (' + r.size_human + ', ' + r.files + ' 个文件)', 'ok');
			else hint('backup-hint', ' ' + (r && r.error ? r.error : '备份失败'), 'err');
			refreshBackups();
		}).catch(function (e) {
			hint('backup-hint', ' ' + e.message, 'err');
		}).finally(function () {
			setBusy('btn-backup', false, '创建备份');
		});
	}

	function deleteBackup(path) {
		ouConfirm({
			tone: 'danger',
			eyebrow: '配置备份',
			title: '删除备份文件',
			message: '删除后不可恢复。',
			detail: path,
			confirmText: '删除备份'
		}).then(function (ok) {
			if (!ok) return;
			ubus('delete_backup', { path: path }).then(function (r) {
				if (r && r.ok) hint('backup-hint', ' 已删除 ' + path, 'ok');
				else hint('backup-hint', ' ' + (r && r.error ? r.error : '删除失败'), 'err');
				refreshBackups();
			}).catch(function (e) {
				hint('backup-hint', ' ' + e.message, 'err');
			});
		});
	}

	function downloadBackup() {
		ubus('list_backups').then(function (r) {
			if (!r.backups || !r.backups.length) {
				hint('backup-hint', '暂无备份可下载', 'err');
				return;
			}
			var b = r.backups[0];
			var form = document.createElement('form');
			form.method = 'POST';
			form.action = '/cgi-bin/cgi-download';
			form.style.display = 'none';
			form.innerHTML =
				'<input type="hidden" name="sessionid" value="' + esc(SID) + '">' +
				'<input type="hidden" name="path" value="' + esc(b.path) + '">' +
				'<input type="hidden" name="filename" value="' + esc(b.name) + '">';
			document.body.appendChild(form);
			form.submit();
			setTimeout(function () { form.remove(); }, 5000);
		}).catch(function (e) {
			hint('backup-hint', ' ' + e.message, 'err');
		});
	}

	function autoRestore() {
		ubus('list_backups').then(function (r) {
			if (!r.backups || !r.backups.length) {
				hint('backup-hint', '未找到 /root/ 下的备份文件', 'err');
				return;
			}
			var b = r.backups[0];
			ouConfirm({
				tone: 'warning',
				eyebrow: '配置恢复',
				title: '从备份恢复配置',
				message: '将使用 tar 解压覆盖 / 下所有配置文件，建议恢复后重启路由器。',
				detail: b.path,
				confirmText: '开始恢复'
			}).then(function (ok) {
				if (!ok) return;
				setBusy('btn-auto-restore', true, '恢复中...');
				ubus('restore', { path: b.path }).then(function (res) {
					if (res && res.ok) hint('backup-hint', ' 配置已恢复,建议重启路由器', 'ok');
					else hint('backup-hint', ' ' + (res && res.error ? res.error : '恢复失败'), 'err');
				}).catch(function (e) {
					hint('backup-hint', ' ' + e.message, 'err');
				}).finally(function () {
					setBusy('btn-auto-restore', false, '自动恢复');
				});
			});
		}).catch(function (e) {
			hint('backup-hint', ' ' + e.message, 'err');
		});
	}

	function manualRestorePick() { $('manual-backup-file').click(); }

	function manualRestoreUpload(ev) {
		var file = ev.target.files[0];
		ev.target.value = '';
		if (!file) return;
		if (!/\.(tar\.gz|tgz|gz)$/i.test(file.name)) {
			hint('backup-hint', ' 请选择 .tar.gz 格式的备份文件', 'err');
			return;
		}
		ouConfirm({
			tone: 'warning',
			eyebrow: '手动恢复',
			title: '上传并恢复配置',
			message: '上传后会立即使用该备份覆盖当前配置，建议恢复后重启路由器。',
			detail: file.name + ' · ' + fmtSize(file.size),
			confirmText: '上传并恢复'
		}).then(function (ok) {
			if (!ok) return;
			var fd = new FormData();
			fd.append('sessionid', SID);
			fd.append('filename', '/tmp/restore-upload.tar.gz');
			fd.append('filemode', '0600');
			fd.append('filedata', file, file.name);
			setBusy('btn-manual-restore', true, '上传中...');
			hint('backup-hint', '正在上传备份文件...');
			fetch('/cgi-bin/cgi-upload', { method: 'POST', credentials: 'same-origin', body: fd })
				.then(function (r) { return r.json(); })
				.then(function (reply) {
					if (reply && reply.failure) throw new Error(reply.failure || '上传失败');
					hint('backup-hint', '上传完成,正在恢复...');
					return ubus('restore', { path: '/tmp/restore-upload.tar.gz' });
				})
				.then(function (res) {
					if (res && res.ok) hint('backup-hint', ' 配置已从上传的备份恢复,建议重启路由器', 'ok');
					else hint('backup-hint', ' ' + (res && res.error ? res.error : '恢复失败'), 'err');
				})
				.catch(function (e) {
					hint('backup-hint', ' ' + e.message, 'err');
				})
				.finally(function () {
					setBusy('btn-manual-restore', false, '手动恢复');
				});
		});
	}

	/* ---------------- 配置 ---------------- */

	function fillConfig(settings) {
		$('cfg-repo').value = settings.repo || '';
		$('cfg-tag').value = settings.tag || '';
		$('cfg-pattern').value = settings.firmware_pattern || '';
		$('cfg-minsize').value = (settings.min_size_mb == null || settings.min_size_mb === '')
			? '15' : settings.min_size_mb;
		$('cfg-proxy').value = settings.proxy || '';
		$('cfg-workers').value = settings.dl_workers || '6';
		$('cfg-chunk').value = settings.dl_chunk_mb || '4';
		$('cfg-stall').value = settings.dl_stall_sec || '30';
		$('cfg-retries').value = settings.dl_retries || '3';
		// 慢速保护的 0 是有效取值（表示关闭保护或不设宽限期），
		// 因此不能用 `||` 把它替换成默认值。
		[['cfg-minspeed', settings.dl_min_speed_kb, '32'],
		 ['cfg-slowwin', settings.dl_slow_window_sec, '15'],
		 ['cfg-slowgrace', settings.dl_slow_grace_sec, '10']].forEach(function (f) {
			if ($(f[0])) {
				$(f[0]).value = (f[1] == null || f[1] === '') ? f[2] : f[1];
			}
		});
		if ($('cfg-expected-sha256')) {
			$('cfg-expected-sha256').value = settings.expected_sha256 || '';
		}
		if (settings.repo) {
			$('cfg-url').value = (settings.tag === 'latest' || (settings.tag || '').indexOf('latest@') === 0)
				? ('https://github.com/' + settings.repo)
				: ('https://github.com/' + settings.repo + '/releases/tag/' + settings.tag);
		} else {
			$('cfg-url').value = '';
		}
		updateSummary();
	}

	function parseUrl() {
		var v = $('cfg-url').value.trim();
		if (!v) { hint('config-hint', '请先粘贴链接或填写仓库', 'err'); return; }
		var repo = '', tag = '';
		var m = v.match(/github\.com\/([^\/\s?#]+)\/([^\/\s?#]+?)(?:\.git)?(?:\/|$)/);
		if (m) {
			repo = m[1] + '/' + m[2];
			var t = v.match(/releases\/tag\/([^\/\s?#]+)/);
			if (t) {
				tag = t[1];
			} else if (v.match(/releases\/?($|[?#])/) || v.match(/releases\/latest/)) {
				tag = 'latest';
			}
		} else if (/^[^\/\s]+\/[^\/\s]+$/.test(v)) {
			repo = v;
			tag = 'latest';
		}
		if (!repo) { hint('config-hint', ' 无法识别,支持:Release 链接 / 仓库链接 / owner/repo', 'err'); return; }
		$('cfg-repo').value = repo;
		$('cfg-tag').value = tag || 'latest';
		hint('config-hint', ' 已识别:仓库 ' + repo + ',标签 ' + (tag || 'latest') + '(可点「获取固件列表」选择固件文件)', 'ok');
		updateSummary();
	}

	function getAssetSizeMB(a) {
		if (typeof a.size === 'number' && a.size > 0) {
			return a.size / (1024 * 1024);
		}
		if (typeof a.size_human === 'string') {
			var str = a.size_human.trim();
			var num = parseFloat(str);
			if (isNaN(num)) return 0;
			if (/GB$/i.test(str)) return num * 1024;
			if (/MB$/i.test(str)) return num;
			if (/KB$/i.test(str)) return num / 1024;
			if (/B$/i.test(str)) return num / (1024 * 1024);
		}
		return 0;
	}

	function fetchAssets() {
		var repo = $('cfg-repo').value.trim();
		if (!repo) { hint('config-hint', ' 请先在①填写或识别仓库', 'err'); return; }
		setBusy('btn-fetch-assets', true, '获取中...');
		ubus('probe_assets', { repo: repo, tag: $('cfg-tag').value.trim() || 'latest' })
			.then(function (r) {
				if (!r || !r.ok) throw new Error((r && r.error) || '获取失败');
				setText('asset-resolved', '最新 Release: ' + r.resolved_tag);
				var sel = $('cfg-asset');
				sel.innerHTML = '';

				var minSizeVal = $('cfg-minsize') ? parseInt($('cfg-minsize').value, 10) : 15;
				var minSizeMB = isNaN(minSizeVal) ? 15 : (minSizeVal < 0 ? 0 : minSizeVal);

				var rawAssets = r.assets || [];
				var filteredAssets = rawAssets.filter(function (a) {
					return getAssetSizeMB(a) >= minSizeMB;
				});
				var filteredCount = rawAssets.length - filteredAssets.length;

				if (filteredAssets.length === 0) {
					var o = document.createElement('option');
					o.value = '';
					o.disabled = true;
					o.selected = true;
					if (rawAssets.length > 0) {
						o.textContent = '未发现 ≥ ' + minSizeMB + 'MB 固件 (' + rawAssets.length + ' 个文件均被过滤)';
						hint('config-hint', ' 未发现 ≥ ' + minSizeMB + 'MB 的固件文件 (已过滤 ' + filteredCount + ' 个非固件附件)，可前往高级配置调整最小体积', 'warn');
					} else {
						o.textContent = '此 Release 无附件文件';
						hint('config-hint', ' 该 Release 下未包含任何产物附件', 'warn');
					}
					sel.appendChild(o);
					$('asset-box').style.display = '';
					return;
				}

				var recommendedSelected = false;
				filteredAssets.forEach(function (a) {
					var o = document.createElement('option');
					o.value = a.name;
					var prefix = '';
					if (a.compat) {
						if (a.compat.level === 'compatible') {
							prefix = ' [推荐] ';
						} else if (a.compat.level === 'warning') {
							prefix = ' [需核对] ';
						} else if (a.compat.level === 'incompatible') {
							prefix = ' [不匹配] ';
						}
					}
					o.textContent = prefix + a.name + ' (' + a.size_human + ')';
					o.setAttribute('data-level', a.compat ? a.compat.level : '');
					o.setAttribute('data-reason', a.compat ? a.compat.reason : '');
					if (!recommendedSelected && a.compat && a.compat.can_upgrade) {
						o.selected = true;
						recommendedSelected = true;
					}
					sel.appendChild(o);
				});
				$('asset-box').style.display = '';
				onAssetSelected();

				var msg = ' 已获取 ' + filteredAssets.length + ' 个固件文件';
				if (filteredCount > 0) {
					msg += ' (已自动过滤 ' + filteredCount + ' 个小于 ' + minSizeMB + 'MB 的非固件小文件)';
				}
				msg += '，已自动推荐适配固件，点「使用此文件」可固化规则';
				hint('config-hint', msg, 'ok');
			})
			.catch(function (e) {
				hint('config-hint', ' ' + e.message, 'err');
			})
			.finally(function () {
				setBusy('btn-fetch-assets', false, '获取固件列表');
			});
	}

	function onAssetSelected() {
		var sel = $('cfg-asset');
		if (!sel || !sel.options || !sel.options.length) return;
		var opt = sel.options[sel.selectedIndex];
		if (!opt) return;
		var level = opt.getAttribute('data-level');
		var reason = opt.getAttribute('data-reason');
		if (level === 'incompatible') {
			hint('config-hint', ' 警告：' + reason, 'err');
		} else if (level === 'compatible') {
			hint('config-hint', ' 兼容评估：' + reason + '，可点「使用此文件」生成匹配规则', 'ok');
		} else if (level === 'warning') {
			hint('config-hint', ' 需人工核对：' + reason, 'warn');
		}
	}

	function escapeRegex(s) {
		return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	}

	function applyAsset() {
		var name = $('cfg-asset').value;
		if (!name) return;
		$('cfg-pattern').value = '^' + escapeRegex(name) + '$';
		hint('config-hint', ' 已按所选文件生成匹配规则(文件名固定匹配;如文件名会变化可在高级配置放宽)', 'ok');
		updateSummary();
	}

	function updateSummary() {
		var repo = $('cfg-repo').value.trim();
		var tag = $('cfg-tag').value.trim() || 'latest';
		var pattern = $('cfg-pattern').value.trim();
		var el = $('config-summary');
		if (!el) return;
		if (!repo) { el.textContent = '尚未配置固件源'; return; }
		var tagText = tag === 'latest' ? '自动追踪最新 Release'
			: tag.indexOf('latest@') === 0 ? '追踪最新 Release (仅取标签匹配 ' + tag.slice(7) + ' 的)'
			: '固定标签 ' + tag;
		el.textContent = '当前: ' + repo + '  ·  ' + tagText +
			(pattern ? '  ·  固件匹配 ' + pattern : '  ·  未设匹配规则');
	}

	function saveConfig() {
		setBusy('btn-save-config', true, '保存中...');
		var cfg = {
			repo: $('cfg-repo').value.trim(),
			tag: $('cfg-tag').value.trim(),
			pattern: $('cfg-pattern').value.trim(),
			proxy: $('cfg-proxy').value.trim(),
			github_token: $('cfg-token').value,
			expected_sha256: $('cfg-expected-sha256') ? $('cfg-expected-sha256').value.trim() : ''
		};
		[['dl_workers', 'cfg-workers'], ['dl_chunk_mb', 'cfg-chunk'],
		 ['dl_stall_sec', 'cfg-stall'], ['dl_retries', 'cfg-retries'],
		 ['min_size_mb', 'cfg-minsize'], ['dl_min_speed_kb', 'cfg-minspeed'],
		 ['dl_slow_window_sec', 'cfg-slowwin'], ['dl_slow_grace_sec', 'cfg-slowgrace']]
			.forEach(function (f) {
				if (!$(f[1])) return;
				var v = parseInt($(f[1]).value, 10);
				if (!isNaN(v)) cfg[f[0]] = v;
			});
		ubus('set_config', cfg).then(function (r) {
			if (r && r.ok) {
				hint('config-hint', ' 配置已保存', 'ok');
				updateSummary();
			} else {
				hint('config-hint', ' ' + (r && r.error ? r.error : '保存失败'), 'err');
			}
		}).catch(function (e) {
			hint('config-hint', ' ' + e.message, 'err');
		}).finally(function () {
			setBusy('btn-save-config', false, '保存配置');
		});
	}

	function renderSysinfo(info) {
		setText('cur-ver', (info.id || 'OpenWrt') + ' ' + (info.release || '') +
			(info.revision ? ' (' + info.revision + ')' : ''));
		var devSub = $('cur-device-sub');
		if (devSub) {
			var devParts = [];
			if (info.model) devParts.push(info.model);
			if (info.arch || info.uname_arch) devParts.push(info.arch || info.uname_arch);
			if (info.target) devParts.push(info.target);
			devSub.textContent = devParts.length ? ('本机: ' + devParts.join(' · ')) : '本机系统固件环境';
		}
	}

	/* ---------------- 日志 ---------------- */

	function refreshLog() {
		setBusy('btn-refresh-log', true);
		ubus('get_log').then(function (r) {
			var text = (r && r.log) ? r.log : '日志为空';
			if (text.length > 2097152) {
				text = text.substring(text.length - 2097152);
			}
			var lv = $('log-view');
			if (lv) {
				lv.textContent = text;
				lv.scrollTop = lv.scrollHeight;
			}
		}).catch(function (e) {
			var lv = $('log-view');
			if (lv) lv.textContent = '日志读取失败: ' + e.message;
		}).finally(function () {
			setBusy('btn-refresh-log', false);
		});
	}

	function clearLog() {
		ouConfirm({
			tone: 'warning',
			eyebrow: '运行日志',
			title: '清空当前日志',
			message: '日志文件内容会被立即清空，且无法恢复。',
			confirmText: '清空日志'
		}).then(function (ok) {
			if (!ok) return;
			setBusy('btn-clear-log', true, '清空中...');
			ubus('clear_log').then(function (r) {
				if (r && r.ok) {
					var lv = $('log-view');
					if (lv) lv.textContent = '日志已清空';
				}
			}).catch(function (e) {
				ouAlert({
					tone: 'danger',
					eyebrow: '运行日志',
					title: '清空日志失败',
					message: e.message
				});
			}).finally(function () {
				setBusy('btn-clear-log', false, '清空日志');
			});
		});
	}

	/* ---------------- 插件自身版本与更新 ---------------- */

	var pluginStatusTimer = null;
	var pluginBaseline = { pid: null, ts: 0 };
	var pluginFresh = false;
	var pluginTicks = 0;
	var lastPluginCheck = null;

	function pluginBadge(text, cls) {
		var b = $('plugin-badge');
		if (!b) return;
		b.className = 'ou-status-pill ' + cls;
		b.innerHTML = '<span class="ou-dot ' + cls + '"></span> ' + esc(text);
	}

	function pluginHint(text, cls) {
		var el = $('plugin-hint');
		if (!el) return;
		el.textContent = text || '';
		el.className = 'ou-plugin-hint' + (cls ? ' ' + cls : '');
	}

	function setPluginProgress(pct, show) {
		var wrap = $('plugin-progress');
		if (wrap) wrap.style.display = show ? '' : 'none';
		var bar = $('plugin-progress-bar');
		if (bar) bar.style.width = Math.max(0, Math.min(100, Number(pct) || 0)) + '%';
		var txt = $('plugin-progress-pct');
		if (txt) txt.textContent = Math.max(0, Math.min(100, Number(pct) || 0)) + '%';
	}

	function pluginVersionText(v) {
		if (!v) return '未知';
		v = String(v);
		return v.charAt(0) === 'v' ? v : 'v' + v;
	}

	// 当前页面模板写入的静态资源版本戳（style.css?v=）。
	// 自更新安装后新的版本文件会变化，可据此判断页面是否仍在用旧资源。
	function assetVersion() {
		var link = document.querySelector('link[href*="online-upgrade/style.css?v="]') ||
			document.querySelector('script[src*="online-upgrade/app.js?v="]');
		var url = link ? (link.href || link.src || '') : '';
		var m = url.match(/[?&]v=([^&]+)/);
		return m ? decodeURIComponent(m[1]) : '';
	}

	function refreshPluginAfterUpdate(result, ts) {
		var r = result || {};
		setPluginProgress(100, true);
		pluginBadge('已更新', 'ok');
		pluginHint(r.message || '插件已更新完成，刷新页面即可加载新界面；无需重启路由器', 'ok');
		setText('plugin-current', pluginVersionText(r.installed_version || r.to || ''));
		setText('plugin-latest', r.to ? 'v' + r.to : '—');
		setText('plugin-checked', fmtTime(ts));
		showBtn('btn-plugin-update', false);
		showBtn('btn-plugin-reload', true);
		lastPluginCheck = null;

		// 重新读取安装后的本地版本；同时对比模板资源版本与后端版本戳。
		// 若不一致，明确提示刷新页面，避免仍显示更新前的旧界面状态。
		ubus('plugin_info').then(function (info) {
			if (info && info.local) {
				setText('plugin-current', pluginVersionText(info.local.display));
			}
			if (info) {
				setText('plugin-env', (info.manager || '未检测到包管理器') + ' · ' +
					(info.pkg_arch || info.arch || '未知架构'));
			}
			var pageVer = assetVersion();
			var installedVer = info && info.cache_version;
			if (pageVer && installedVer && pageVer !== installedVer) {
				pluginHint('插件已更新到 ' + pluginVersionText(installedVer || r.to) +
					'；当前页面资源仍是 v' + pageVer +
					'，请刷新页面后新界面才会生效。', 'warn');
			}
		}).catch(function () { /* 版本读取失败不影响“已更新”终态 */ });

		refreshTasks();
	}

	function stopPluginPoll() {
		if (pluginStatusTimer) {
			clearInterval(pluginStatusTimer);
			pluginStatusTimer = null;
		}
	}

	function snapshotPluginStatus() {
		return ubus('get_plugin_status').then(function (st) {
			pluginBaseline = {
				pid: (st && st.pid != null) ? st.pid : null,
				ts: Number((st && st.ts) || 0)
			};
		}).catch(function () {
			pluginBaseline = { pid: null, ts: 0 };
		});
	}

	function pluginIsFresh(st) {
		if (!st) return false;
		if (st.pid != null && pluginBaseline.pid != null)
			return st.pid !== pluginBaseline.pid;
		return Number(st.ts || 0) > pluginBaseline.ts;
	}

	function renderPluginInfo(info) {
		if (!info || info.ok === false) {
			pluginBadge('读取失败', 'err');
			pluginHint('插件信息读取失败，请刷新页面重试', 'err');
			return;
		}
		var local = info.local || {};
		var cur = $('plugin-current');
		if (cur) {
			cur.textContent = pluginVersionText(local.display || '未知');
			cur.title = (local.source_text || local.source || '') +
				(local.pkg_release ? '（发布号 r' + local.pkg_release + '）' : '');
		}
		setText('plugin-env', (info.manager || '未检测到包管理器') + ' · ' +
			(info.pkg_arch || info.arch || '未知架构'));
		var asset = $('plugin-asset');
		if (asset && !lastPluginCheck) {
			asset.innerHTML = '更新源：<span class="ou-fact-strong">' + esc(info.repo || '') + '</span><br>' +
				'<span class="ou-plugin-asset-sub">固定读取稳定 Release，按 ' + esc(info.manager || '包管理器') +
				' 与本机 ' + esc(info.pkg_arch || '架构') + ' 选择安装包。</span>';
		}
		if (!info.supported) {
			pluginBadge('环境不支持', 'err');
			pluginHint(info.reason || '当前环境无法自动更新插件', 'err');
			return;
		}
	}

	function loadPluginInfo() {
		return ubus('plugin_info').then(renderPluginInfo).catch(function (e) {
			pluginBadge('读取失败', 'err');
			pluginHint('插件信息读取失败: ' + e.message, 'err');
		});
	}

	function renderPluginCheckResult(res) {
		lastPluginCheck = res;
		if (!res) return;
		var latest = res.latest || {};
		if (res.local && res.local.display) {
			setText('plugin-current', pluginVersionText(res.local.display));
		}
		setText('plugin-latest', latest.version ? 'v' + latest.version : '—');
		setText('plugin-checked', fmtTime(res.checked_ts));

		var assetEl = $('plugin-asset');
		if (assetEl) {
			if (res.update_available && res.asset) {
				var a = res.asset;
				assetEl.innerHTML =
					'<span class="ou-fact-strong">' + esc(a.name || '安装包') + '</span><br>' +
					'<span class="ou-plugin-asset-sub">' + esc(a.size_human || '') +
					' · ' + esc(a.arch || '') + ' · SHA256 ' + esc((a.sha256 || '').substring(0, 16)) +
					'…</span>';
			} else if (res.latest) {
				assetEl.innerHTML = '<span class="ou-fact-strong">' + esc(latest.tag || '') + '</span><br>' +
					'<span class="ou-plugin-asset-sub">' + esc(latest.published_at || '—') +
					' · 无可用更新安装包</span>';
			}
		}

		if (res.ok !== true) {
			pluginBadge('检查失败', 'err');
			pluginHint(res.reason || '插件检查失败', 'err');
			showBtn('btn-plugin-update', false);
			return;
		}

		if (res.update_available) {
			pluginBadge('发现新版本', 'update');
			pluginHint(res.reason || ('可更新到 v' + latest.version), 'warn');
			showBtn('btn-plugin-update', true);
			setBusy('btn-plugin-update', false, '更新插件');
		} else {
			pluginBadge('已是最新', 'ok');
			pluginHint(res.reason || '当前已是最新稳定版', 'ok');
			showBtn('btn-plugin-update', false);
		}
		setBusy('btn-plugin-check', false, '检查插件更新');
	}

	function renderPluginStatus(st) {
		if (!st || st.state === 'idle') return false;
		var state = st.state;
		var active = state === 'checking' || state === 'updating' ||
			state === 'downloading' || state === 'verifying' || state === 'installing';

		if (active) {
			setPluginProgress(st.pct || 0, true);
			var label = state === 'checking' ? '检查中' :
				state === 'downloading' ? '下载中' :
				state === 'verifying' ? '校验中' :
				state === 'installing' ? '安装中' : '更新中';
			pluginBadge(label, 'update');
			pluginHint(st.msg || label, '');
			setBusy('btn-plugin-check', true, '检查插件更新');
			setBusy('btn-plugin-update', true, '更新插件');
			showBtn('btn-plugin-update', false);
			showBtn('btn-plugin-reload', false);
			return true;
		}

		setBusy('btn-plugin-check', false, '检查插件更新');
		setBusy('btn-plugin-update', false, '更新插件');

		if (state === 'checked') {
			setPluginProgress(100, false);
			renderPluginCheckResult(st.result);
			return false;
		}
		if (state === 'check_failed') {
			setPluginProgress(100, false);
			pluginBadge('检查失败', 'err');
			pluginHint(st.error || st.msg || '插件检查失败', 'err');
			showBtn('btn-plugin-update', false);
			return false;
		}
		if (state === 'done') {
			refreshPluginAfterUpdate(st.result, st.ts);
			return false;
		}
		if (state === 'failed') {
			setPluginProgress(st.pct || 0, true);
			pluginBadge('更新失败', 'err');
			pluginHint(st.error || st.msg || '插件更新失败', 'err');
			if (lastPluginCheck && lastPluginCheck.update_available) {
				showBtn('btn-plugin-update', true);
			} else {
				showBtn('btn-plugin-update', false);
			}
			refreshTasks();
			return false;
		}
		return false;
	}

	function startPluginPoll() {
		stopPluginPoll();
		pluginTicks = 0;
		pluginStatusTimer = setInterval(function () {
			pluginTicks++;
			if (pluginTicks > 90) {
				stopPluginPoll();
				pluginBadge('状态未知', 'err');
				pluginHint('未读取到本次插件任务的状态，请查看运行日志后重试', 'err');
				setBusy('btn-plugin-check', false, '检查插件更新');
				return;
			}
			ubus('get_plugin_status').then(function (st) {
				if (pluginFresh) {
					if (!pluginIsFresh(st)) return;
					pluginFresh = false;
				}
				if (!renderPluginStatus(st)) stopPluginPoll();
			}).catch(function () { /* 路由重启或瞬时 RPC 错误：继续轮询 */ });
		}, 1000);
	}

	function runPluginCheck() {
		setBusy('btn-plugin-check', true, '检查中...');
		setBusy('btn-plugin-update', true, '更新插件');
		showBtn('btn-plugin-update', false);
		showBtn('btn-plugin-reload', false);
		setPluginProgress(0, false);
		pluginBadge('检查中', 'update');
		pluginHint('正在获取插件 Release 信息...', '');
		stopPluginPoll();
		snapshotPluginStatus().then(function () {
			pluginFresh = true;
			return ubus('start_plugin_check');
		}).then(function (r) {
			if (r && r.ok === false) {
				pluginFresh = false;
				pluginBadge('检查失败', 'err');
				pluginHint(r.error || '插件检查未能启动', 'err');
				setBusy('btn-plugin-check', false, '检查插件更新');
				return;
			}
			startPluginPoll();
		}).catch(function (e) {
			pluginFresh = false;
			pluginBadge('检查失败', 'err');
			pluginHint('插件检查请求失败: ' + e.message, 'err');
			setBusy('btn-plugin-check', false, '检查插件更新');
		});
	}

	function runPluginUpdate() {
		var r = lastPluginCheck;
		if (!r || !r.update_available || !r.asset) return;
		var from = r.local ? r.local.display : '未知';
		var to = r.latest ? r.latest.version : '未知';
		ouConfirm({
			tone: 'primary',
			eyebrow: '插件更新',
			title: '更新插件到 v' + to,
			message: '当前版本：' + from + '\n更新只替换插件本身，不会重启路由器；完成后刷新页面即可。',
			detail: r.asset.name + '\nSHA256: ' + (r.asset.sha256 || '').substring(0, 16) + '...',
			confirmText: '开始更新'
		}).then(function (ok) {
			if (!ok) return;
			setBusy('btn-plugin-check', true, '检查插件更新');
			setBusy('btn-plugin-update', true, '启动中...');
			showBtn('btn-plugin-update', false);
			showBtn('btn-plugin-reload', false);
			setPluginProgress(0, true);
			pluginBadge('更新中', 'update');
			pluginHint('正在校验最新版本并准备下载...', '');
			stopPluginPoll();
			refreshTasks();
			snapshotPluginStatus().then(function () {
				pluginFresh = true;
				return ubus('start_plugin_update');
			}).then(function (res) {
				if (res && res.ok === false) {
					pluginFresh = false;
					pluginBadge('更新失败', 'err');
					pluginHint(res.error || '插件更新未能启动', 'err');
					setBusy('btn-plugin-check', false, '检查插件更新');
					setBusy('btn-plugin-update', false, '更新插件');
					showBtn('btn-plugin-update', true);
					return;
				}
				startPluginPoll();
			}).catch(function (e) {
				pluginFresh = false;
				pluginBadge('更新失败', 'err');
				pluginHint('插件更新请求失败: ' + e.message, 'err');
				setBusy('btn-plugin-check', false, '检查插件更新');
				setBusy('btn-plugin-update', false, '更新插件');
				showBtn('btn-plugin-update', true);
			});
		});
	}

	/* ---------------- 手机端信息密度 ---------------- */

	// 手机端默认收起下载任务、备份、仓库配置与运行日志，只保留标题和展开入口。
	// 桌面端完全不受影响；按钮由 JS 动态插入，模板里不增加额外结构。
	var MOBILE_MQ = window.matchMedia('(max-width: 760px)');
	var MOBILE_SECTIONS = [
		['card-tasks', '下载任务'],
		['card-backup', '备份与恢复'],
		['card-config', '仓库配置'],
		['card-log', '运行日志']
	];

	function applyMobileSections() {
		MOBILE_SECTIONS.forEach(function (item) {
			var card = $(item[0]);
			if (!card) return;
			var head = card.querySelector('.ou-card-head');
			var btn = card.querySelector('.ou-mobile-toggle');
			if (!btn && head) {
				btn = document.createElement('button');
				btn.type = 'button';
				btn.className = 'ou-mobile-toggle';
				card.insertBefore(btn, head.nextSibling);
				btn.addEventListener('click', function () {
					var collapsed = card.classList.toggle('ou-mobile-collapsed');
					card.setAttribute('data-mobile-expanded', collapsed ? '0' : '1');
					btn.textContent = (collapsed ? '展开' : '收起') + item[1];
				});
			}
			if (!btn) return;
			if (!MOBILE_MQ.matches) {
				card.classList.remove('ou-mobile-collapsed');
				btn.style.display = 'none';
				return;
			}
			btn.style.display = '';
			var expanded = card.getAttribute('data-mobile-expanded') === '1';
			card.classList.toggle('ou-mobile-collapsed', !expanded);
			btn.textContent = (expanded ? '收起' : '展开') + item[1];
		});
	}

	if (MOBILE_MQ.addEventListener) {
		MOBILE_MQ.addEventListener('change', applyMobileSections);
	} else if (MOBILE_MQ.addListener) {
		MOBILE_MQ.addListener(applyMobileSections);
	}

	/* ---------------- 初始化 ---------------- */

	function boot() {
		$('btn-check').addEventListener('click', runCheck);
		$('btn-upgrade').addEventListener('click', function () { startUpgrade(false); });
		$('btn-force').addEventListener('click', function () { startUpgrade(true); });
		$('btn-backup').addEventListener('click', runBackup);
		$('btn-download').addEventListener('click', downloadBackup);
		$('btn-auto-restore').addEventListener('click', autoRestore);
		$('btn-manual-restore').addEventListener('click', manualRestorePick);
		$('manual-backup-file').addEventListener('change', manualRestoreUpload);
		$('btn-parse-url').addEventListener('click', parseUrl);
		$('btn-fetch-assets').addEventListener('click', fetchAssets);
		$('cfg-asset').addEventListener('change', onAssetSelected);
		$('btn-apply-asset').addEventListener('click', applyAsset);
		$('btn-save-config').addEventListener('click', saveConfig);
		$('btn-refresh-log').addEventListener('click', refreshLog);
		$('btn-clear-log').addEventListener('click', clearLog);
		$('btn-start-download').addEventListener('click', startDownload);
		$('btn-pause-download').addEventListener('click', function () { taskCommand('pause_download', '暂停'); });
		$('btn-resume-download').addEventListener('click', function () { taskCommand('resume_download', '继续'); });
		$('btn-cancel-download').addEventListener('click', function () { taskCommand('cancel_download', '取消'); });
		$('btn-reload').addEventListener('click', function () { location.reload(); });
		$('btn-plugin-check').addEventListener('click', runPluginCheck);
		$('btn-plugin-update').addEventListener('click', runPluginUpdate);
		$('btn-plugin-reload').addEventListener('click', function () { location.reload(); });
		$('evidence-toggle').addEventListener('click', function () {
			setEvidenceExpanded(!$('check-evidence').classList.contains('is-open'));
		});
		bindDialog();
		applyMobileSections();

		document.querySelectorAll('.ou-quick-pill').forEach(function (pill) {
			pill.addEventListener('click', function () {
				var p = this.getAttribute('data-proxy');
				$('cfg-proxy').value = p;
				hint('config-hint', p ? ('已填入代理节点: ' + p) : '已切换为 GitHub 直连模式', 'ok');
			});
		});

		refreshLog();
		loadPluginInfo();
		ubus('get_plugin_status').then(function (st) {
			if (renderPluginStatus(st)) startPluginPoll();
		}).catch(function () {});
		ubus('get_sysinfo').then(renderSysinfo).catch(function () { setText('cur-ver', '读取失败'); });
		ubus('get_config').then(function (r) { fillConfig(r.settings || {}); }).catch(function (e) {
			hint('config-hint', ' 读取配置失败: ' + e.message, 'err');
		});
		refreshBackups();
		refreshTasks();

		// 若已有任务在运行（例如升级途中刷新了页面），恢复对应的界面状态
		ubus('get_status').then(function (st) {
			if (st.stage === 'idle' || st.stage === 'check_done' || st.stage === 'check_failed') {
				if (st.stage === 'check_done' && st.result) renderCheckResult(st.result);
				return;
			}
			if (st.stage === 'flashing') { showRebootOverlay(); return; }
			if (st.stage === 'downloading' || st.stage === 'paused' || st.stage === 'downloaded' ||
				st.stage === 'saving' || st.stage === 'backing_up' || st.stage === 'backup_done') {
				renderProgress(st);
				pollUpgrade();
			}
		}).catch(function () {});
	}

	if (document.readyState === 'loading')
		document.addEventListener('DOMContentLoaded', boot);
	else
		boot();
})();
