/* luci-app-online-upgrade v2 — native JS frontend (no LuCI framework).
 * RPC: uhttpd-mod-ubus JSON-RPC against the luci.online-upgrade object.
 * Session token: LuCI admin session cookie (the dispatcher session IS an
 * ubus session), with a server-injected global as a fallback hook. */
(function () {
	'use strict';

	/* ---------------- session ---------------- */

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

	/* ---------------- ubus JSON-RPC ---------------- */

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

	/* ---------------- helpers ---------------- */

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

	/* ---------------- detached run freshness ---------------- */

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

	/* ---------------- status card ---------------- */

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

	function renderCompatDetail(ci, res) {
		var html = '<span class="ou-reason-text">' + esc(res.reason) + '</span>';
		if (ci) {
			var lv = compatLevel(ci);
			var chipCls = lv === 'compatible' ? 'ou-chip-ok'
				: (lv === 'warning' || lv === 'unknown') ? 'ou-chip-warn' : 'ou-chip-err';
			html += '<span class="ou-tag-chip ' + chipCls + '" title="' + esc(ci.reason || '') + '">' +
				esc(ci.badge || '适配评估') + '</span>';
			var checks = ci.checks || {};
			html += '<span class="ou-tag-chip" title="资产架构 ' + esc(ci.asset_arch || '未声明') +
				' / 本机架构 ' + esc(ci.device_arch || '未知') + '">架构: ' +
				esc(ARCH_CHECK_TEXT[checks.arch] || checks.arch || '未比对') + '</span>';
			html += '<span class="ou-tag-chip" title="' + esc(ci.reason || '') + '">机型: ' +
				esc(MODEL_CHECK_TEXT[checks.model] || checks.model || '未比对') + '</span>';
		html += '<span class="ou-tag-chip" title="本机标识 ' + esc(ci.detected_model || '') +
				' / 架构 ' + esc(ci.detected_arch || '') + '">本机: ' +
				esc(ci.detected_model || '未知') + ' / ' + esc(ci.detected_arch || '未知') + '</span>';
		}
		if (ci && ci.matched_rule) {
			html += '<span class="ou-tag-chip ou-chip-ok" title="' + esc(ci.matched_rule) + '">规则: ' +
				esc(ci.matched_rule) + '</span>';
		}
		var ver = res.version || null;
		if (ver && ver.relation) {
			html += '<span class="ou-tag-chip">' + esc(RELATION_TEXT[ver.relation] || ver.relation) + '</span>';
		}
		if (ver && (ver.local || ver.local_source_text)) {
			// 本地版本与来源必须一起给出：用户才知道「当前版本」这个结论是怎么来的。
			var srcTitle = (ver.local_source_text || ver.local_source || '来源未知') +
				(ver.local_confidence ? '（可信度：' + (CONFIDENCE_TEXT[ver.local_confidence] || ver.local_confidence) + '）' : '');
			var localCls = ver.local_confidence === 'strong' ? 'ou-chip-ok'
				: (ver.local_confidence === 'weak' ? 'ou-chip-warn' : '');
			html += '<span class="ou-tag-chip ' + localCls + '" title="' + esc(srcTitle) + '">本地版本: ' +
				esc(ver.local || '无法取到') + '</span>';
			if (ver.local_source_text) {
				html += '<span class="ou-tag-chip" title="' + esc(srcTitle) + '">依据: ' +
					esc(ver.local_source_text) + '</span>';
			}
		}
		if (ver && ver.state) {
			html += '<span class="ou-tag-chip ' + (ver.state === 'decidable' ? 'ou-chip-ok' : 'ou-chip-warn') +
				'" title="' + esc(STATE_DESC[ver.state] || '') + '">' +
				esc(STATE_TEXT[ver.state] || ver.state) + '</span>';
		}
		if (res.latest && res.latest.sha256) {
			html += '<span class="ou-tag-chip" title="完整 SHA256: ' + esc(res.latest.sha256) + '">SHA256: ' +
				esc(res.latest.sha256.substring(0, 10)) + '...</span>';
		}
		if (res.release_url) {
			html += '<a href="' + esc(res.release_url) + '" target="_blank" rel="noopener" class="ou-ext-link">' +
				'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="ou-ext-svg"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>' +
				'<span>查看 Release 发布页</span></a>';
		}
		if (ci && ci.basis && ci.basis.length) {
			html += '<div class="ou-basis-list">';
			ci.basis.forEach(function (b) {
				html += '<div class="ou-basis-item">' + esc(b) + '</div>';
			});
			html += '</div>';
		}
		return html;
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
			setText('check-reason', errText);
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

		var reasonEl = $('check-reason');
		if (reasonEl) reasonEl.innerHTML = renderCompatDetail(ci, res);

		var hasUpdate = res.has_update === true;
		var ver = res.version || null;
		var sameVersion = !!(ver && ver.relation === 'same');
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
			// 只保留显式点击的强制升级（同版本重装）。后端 reason 已说明重装走强制升级。
			msg = res.reason || '已是最新版本（与当前固件同版本）';
			if (ver.state === 'suspect') msg += '。本地版本依据较弱，仅供参考';
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
				msg = '固件兼容，发现新版本可用，可点击「立即升级」开始更新。';
				lvl = 'ok';
				if (badge) {
					badge.className = 'ou-status-pill update';
					badge.innerHTML = '<span class="ou-dot update"></span> 有新版本';
				}
			}
		} else {
			msg = res.reason || '已是最新版本';
			lvl = 'ok';
			if (badge) {
				badge.className = 'ou-status-pill ok';
				badge.innerHTML = '<span class="ou-dot ok"></span> 已是最新';
			}
		}

		hint('check-result', msg, lvl);
		if (box) box.className = 'ou-notice-box' + (lvl === 'ok' ? ' ok' : (lvl === 'err' ? ' err' : ''));
		showBtn('btn-upgrade', lv === 'compatible' && hasUpdate && !sameVersion);
		if ($('btn-force')) {
			showBtn('btn-force', true);
			$('btn-force').title = sameVersion
				? '当前固件与 Release 同版本：这是同版本重装，不是升级，仅在你需要重刷固件时手动使用'
				: (lv === 'compatible'
					? '即使已是最新版本也重新下载刷写'
					: '跳过适配判定与版本方向检查，强制下载并刷写');
			setBusy('btn-force', false, forceLabel());
		}
	}

	function runCheck() {
		setBusy('btn-check', true, '检查中...');
		setBusy('btn-upgrade', true);
		setBusy('btn-force', true);
		hint('check-result', '正在连接云端检查固件更新...');
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
			if (box) box.className = 'ou-notice-box err';
			finishCheck();
		});
	}

	/* ---------------- upgrade flow ---------------- */

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

		// poll until the router answers again (LAN IP may change → try hostname too)
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
				if (st.stage === 'flashing') {
					showRebootOverlay();
				} else if (st.stage === 'failed') {
					stopPoll();
					setBusy('btn-check', false, '检查更新');
					showBtn('btn-force', true);
					$('card-progress').style.display = 'none';
					hint('check-result', '升级失败: ' + (st.error || '未知错误'), 'err');
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
		var msg = isForce
			? (same
				? '确定同版本重装固件?\n\n当前固件与 Release 为同一版本，这是重装而不是升级。\n强制模式会跳过适配判定与版本方向检查。\n系统仍会备份配置、下载并校验固件后再刷写。\n请勿断电!'
				: '确定强制更新固件?\n\n强制模式会跳过适配判定与版本方向检查(目标固件可能无法确认是否适配本机,或版本低于当前)。\n系统仍会备份配置、下载并校验固件后再刷写。\n请勿断电!')
			: '确定执行在线固件升级?\n\n系统将自动:备份配置 → 多线程下载固件 → 校验 → 刷写(自动恢复配置) → 重启。\n请勿断电!';
		if (!confirm(msg)) return;

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

	/* ---------------- backup & restore ---------------- */

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
		if (!confirm('确定删除备份?\n\n' + path + '\n\n删除后不可恢复!')) return;
		ubus('delete_backup', { path: path }).then(function (r) {
			if (r && r.ok) hint('backup-hint', ' 已删除 ' + path, 'ok');
			else hint('backup-hint', ' ' + (r && r.error ? r.error : '删除失败'), 'err');
			refreshBackups();
		}).catch(function (e) {
			hint('backup-hint', ' ' + e.message, 'err');
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
			if (!confirm('确定从备份恢复配置?\n\n' + b.path + '\n\n将使用 tar 解压覆盖 / 下所有配置文件,建议恢复后重启。')) return;
			setBusy('btn-auto-restore', true, '恢复中...');
			ubus('restore', { path: b.path }).then(function (res) {
				if (res && res.ok) hint('backup-hint', ' 配置已恢复,建议重启路由器', 'ok');
				else hint('backup-hint', ' ' + (res && res.error ? res.error : '恢复失败'), 'err');
			}).catch(function (e) {
				hint('backup-hint', ' ' + e.message, 'err');
			}).finally(function () {
				setBusy('btn-auto-restore', false, '自动恢复');
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
		if (!confirm('确定上传并恢复配置?\n\n文件: ' + file.name + ' (' + fmtSize(file.size) + ')')) return;

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
	}

	/* ---------------- config ---------------- */

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
		 ['min_size_mb', 'cfg-minsize']].forEach(function (f) {
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

	/* ---------------- log ---------------- */

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
		if (!confirm('确定清空当前运行日志吗？')) return;
		setBusy('btn-clear-log', true, '清空中...');
		ubus('clear_log').then(function (r) {
			if (r && r.ok) {
				var lv = $('log-view');
				if (lv) lv.textContent = '日志已清空';
			}
		}).catch(function (e) {
			alert('清空日志失败: ' + e.message);
		}).finally(function () {
			setBusy('btn-clear-log', false, '清空日志');
		});
	}

	/* ---------------- boot ---------------- */

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
		$('btn-reload').addEventListener('click', function () { location.reload(); });

		document.querySelectorAll('.ou-quick-pill').forEach(function (pill) {
			pill.addEventListener('click', function () {
				var p = this.getAttribute('data-proxy');
				$('cfg-proxy').value = p;
				hint('config-hint', p ? ('已填入代理节点: ' + p) : '已切换为 GitHub 直连模式', 'ok');
			});
		});

		refreshLog();
		ubus('get_sysinfo').then(renderSysinfo).catch(function () { setText('cur-ver', '读取失败'); });
		ubus('get_config').then(function (r) { fillConfig(r.settings || {}); }).catch(function (e) {
			hint('config-hint', ' 读取配置失败: ' + e.message, 'err');
		});
		refreshBackups();

		// resume UI if an operation is already running (e.g. page reloaded mid-upgrade)
		ubus('get_status').then(function (st) {
			if (st.stage === 'idle' || st.stage === 'check_done' || st.stage === 'check_failed') {
				if (st.stage === 'check_done' && st.result) renderCheckResult(st.result);
				return;
			}
			if (st.stage === 'flashing') { showRebootOverlay(); return; }
			if (st.stage === 'downloading' || st.stage === 'downloaded' || st.stage === 'saving' || st.stage === 'backing_up' || st.stage === 'backup_done') {
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
