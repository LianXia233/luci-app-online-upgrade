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

	/* ---------------- status card ---------------- */

	var lastCheck = null;

	function renderCheckResult(res) {
		lastCheck = res;
		var badge = $('status-badge');
		var box = $('check-result-box');
		if (!res || res.ok !== true) {
			setText('check-result', '❌ ' + (res && res.error ? res.error : '检测失败'));
			hint('check-result', (res && res.error ? res.error : '检测失败'), 'err');
			if (box) box.className = 'ou-notice-box err';
			if (badge) {
				badge.className = 'ou-status-pill err';
				badge.innerHTML = '<span class="ou-dot err"></span> 检测异常';
			}
			setText('check-reason', (res && res.error) ? res.error : '检测异常');
			$('btn-force').style.display = '';
			$('btn-upgrade').style.display = 'none';
			return;
		}
		setText('latest-name', res.latest.name);
		var verDisplay = res.latest.version ? ('v' + res.latest.version) : (res.resolved_tag || '最新构建');
		setText('new-ver', verDisplay);
		setText('latest-size', res.latest.size_human);
		setText('latest-time', res.latest.updated_local);

		// Render Detection Reason with full details (Reason, Resolved Tag, and direct GitHub Link)
		var reasonEl = $('check-reason');
		if (reasonEl) {
			var reasonHtml = '<span class="ou-reason-text">' + esc(res.reason) + '</span>';
			if (res.resolved_tag) {
				reasonHtml += '<span class="ou-tag-chip"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="ou-chip-svg"><path d="M20.59 13.41l-7.17 7.17a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82z"/><line x1="7" y1="7" x2="7.01" y2="7"/></svg>目标 Release: ' + esc(res.resolved_tag) + '</span>';
			}
			if (res.release_url) {
				reasonHtml += '<a href="' + esc(res.release_url) + '" target="_blank" rel="noopener" class="ou-ext-link">' +
					'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="ou-ext-svg"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>' +
					'<span>查看 Release 发布页 ↗</span></a>';
			}
			reasonEl.innerHTML = reasonHtml;
		}

		var hasUpdate = res.has_update === true;
		setText('check-result', hasUpdate ? '🎉 发现新固件可用，请点击「立即升级」开始更新！' : ('✓ ' + res.reason));
		hint('check-result', hasUpdate ? '🎉 发现新固件可用，请点击「立即升级」开始更新！' : ('✓ ' + res.reason), hasUpdate ? 'ok' : '');
		if (box) box.className = 'ou-notice-box' + (hasUpdate ? ' ok' : '');
		if (badge) {
			if (hasUpdate) {
				badge.className = 'ou-status-pill update';
				badge.innerHTML = '<span class="ou-dot update"></span> 有新版本';
			} else {
				badge.className = 'ou-status-pill ok';
				badge.innerHTML = '<span class="ou-dot ok"></span> 已是最新';
			}
		}
		$('btn-upgrade').style.display = hasUpdate ? '' : 'none';
		$('btn-force').style.display = '';
	}

	function runCheck() {
		setBusy('btn-check', true, '检查中...');
		hint('check-result', '正在连接云端检查固件更新...');
		var box = $('check-result-box');
		if (box) box.className = 'ou-notice-box';
		var badge = $('status-badge');
		if (badge) {
			badge.className = 'ou-status-pill update';
			badge.innerHTML = '<span class="ou-dot update"></span> 检测中';
		}
		stopPoll();
		ubus('check').then(function () {
			function checkTick() {
				ubus('get_status').then(function (st) {
					if (st.stage === 'check_done' && st.result) {
						stopPoll();
						renderCheckResult(st.result);
						refreshLog();
						setBusy('btn-check', false, '检查更新');
					} else if (st.stage === 'check_failed' || st.stage === 'failed') {
						stopPoll();
						renderCheckResult({ ok: false, error: st.error || st.msg || '检测失败' });
						refreshLog();
						setBusy('btn-check', false, '检查更新');
					}
				}).catch(function () { /* transient rpc errors during poll: ignore */ });
			}
			checkTick();
			pollTimer = setInterval(checkTick, 1000);
		}).catch(function (e) {
			hint('check-result', '❌ ' + e.message, 'err');
			if (box) box.className = 'ou-notice-box err';
			setBusy('btn-check', false, '检查更新');
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
		$('progress-text').textContent = (st.error ? ('❌ ' + st.error) : (st.msg || STAGE_TEXT[st.stage] || st.stage));
		updatePipeline(st.stage);
	}

	function pollUpgrade() {
		stopPoll();
		function tick() {
			Promise.all([ubus('get_status'), ubus('get_log')]).then(function (rs) {
				var st = rs[0], lg = rs[1];
				if (lg && lg.log) {
					var lv = $('log-view');
					lv.textContent = lg.log;
					lv.scrollTop = lv.scrollHeight;
				}
				renderProgress(st);
				if (st.stage === 'flashing') {
					showRebootOverlay();
				} else if (st.stage === 'failed') {
					stopPoll();
					setBusy('btn-check', false, '检查更新');
					$('btn-force').style.display = '';
					$('card-progress').style.display = 'none';
					hint('check-result', '❌ 升级失败: ' + (st.error || '未知错误'), 'err');
				}
			}).catch(function () { /* keep polling; device may reboot mid-poll */ });
		}
		tick();
		pollTimer = setInterval(tick, 1000);
	}

	function startUpgrade(isForce) {
		var msg = isForce
			? '确定强制更新固件?\n\n即使已是最新版本也会重新下载并刷写。\n配置将自动备份并在刷写后恢复。\n请勿断电!'
			: '确定执行在线固件升级?\n\n系统将自动:备份配置 → 多线程下载固件 → 刷写(自动恢复配置) → 重启。\n请勿断电!';
		if (!confirm(msg)) return;

		ubus('start_upgrade', { force: isForce === true }).then(function () {
			$('btn-upgrade').style.display = 'none';
			$('btn-force').style.display = 'none';
			$('card-progress').style.display = '';
			pollUpgrade();
		}).catch(function (e) {
			hint('check-result', '❌ ' + e.message, 'err');
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
			hint('backup-hint', '❌ ' + e.message, 'err');
		});
	}

	function runBackup() {
		setBusy('btn-backup', true, '备份中...');
		ubus('backup').then(function (r) {
			if (r && r.ok) hint('backup-hint', '✅ 备份成功: ' + r.path + ' (' + r.size_human + ', ' + r.files + ' 个文件)', 'ok');
			else hint('backup-hint', '❌ ' + (r && r.error ? r.error : '备份失败'), 'err');
			refreshBackups();
		}).catch(function (e) {
			hint('backup-hint', '❌ ' + e.message, 'err');
		}).finally(function () {
			setBusy('btn-backup', false, '创建备份');
		});
	}

	function deleteBackup(path) {
		if (!confirm('确定删除备份?\n\n' + path + '\n\n删除后不可恢复!')) return;
		ubus('delete_backup', { path: path }).then(function (r) {
			if (r && r.ok) hint('backup-hint', '✅ 已删除 ' + path, 'ok');
			else hint('backup-hint', '❌ ' + (r && r.error ? r.error : '删除失败'), 'err');
			refreshBackups();
		}).catch(function (e) {
			hint('backup-hint', '❌ ' + e.message, 'err');
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
			hint('backup-hint', '❌ ' + e.message, 'err');
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
				if (res && res.ok) hint('backup-hint', '✅ 配置已恢复,建议重启路由器', 'ok');
				else hint('backup-hint', '❌ ' + (res && res.error ? res.error : '恢复失败'), 'err');
			}).catch(function (e) {
				hint('backup-hint', '❌ ' + e.message, 'err');
			}).finally(function () {
				setBusy('btn-auto-restore', false, '自动恢复');
			});
		}).catch(function (e) {
			hint('backup-hint', '❌ ' + e.message, 'err');
		});
	}

	function manualRestorePick() { $('manual-backup-file').click(); }

	function manualRestoreUpload(ev) {
		var file = ev.target.files[0];
		ev.target.value = '';
		if (!file) return;
		if (!/\.(tar\.gz|tgz|gz)$/i.test(file.name)) {
			hint('backup-hint', '❌ 请选择 .tar.gz 格式的备份文件', 'err');
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
				if (res && res.ok) hint('backup-hint', '✅ 配置已从上传的备份恢复,建议重启路由器', 'ok');
				else hint('backup-hint', '❌ ' + (res && res.error ? res.error : '恢复失败'), 'err');
			})
			.catch(function (e) {
				hint('backup-hint', '❌ ' + e.message, 'err');
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
		if (!repo) { hint('config-hint', '❌ 无法识别,支持:Release 链接 / 仓库链接 / owner/repo', 'err'); return; }
		$('cfg-repo').value = repo;
		$('cfg-tag').value = tag || 'latest';
		hint('config-hint', '✅ 已识别:仓库 ' + repo + ',标签 ' + (tag || 'latest') + '(可点「获取固件列表」选择固件文件)', 'ok');
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
		if (!repo) { hint('config-hint', '❌ 请先在①填写或识别仓库', 'err'); return; }
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
						hint('config-hint', '⚠️ 未发现 ≥ ' + minSizeMB + 'MB 的固件文件 (已过滤 ' + filteredCount + ' 个非固件附件)，可前往高级配置调整最小体积', 'warn');
					} else {
						o.textContent = '此 Release 无附件文件';
						hint('config-hint', '⚠️ 该 Release 下未包含任何产物附件', 'warn');
					}
					sel.appendChild(o);
					$('asset-box').style.display = '';
					return;
				}

				filteredAssets.forEach(function (a) {
					var o = document.createElement('option');
					o.value = a.name;
					o.textContent = a.name + ' (' + a.size_human + ')';
					sel.appendChild(o);
				});
				$('asset-box').style.display = '';

				var msg = '✅ 已获取 ' + filteredAssets.length + ' 个固件文件';
				if (filteredCount > 0) {
					msg += ' (已自动过滤 ' + filteredCount + ' 个小于 ' + minSizeMB + 'MB 的非固件小文件)';
				}
				msg += '，点选固件后点「使用此文件」';
				hint('config-hint', msg, 'ok');
			})
			.catch(function (e) {
				hint('config-hint', '❌ ' + e.message, 'err');
			})
			.finally(function () {
				setBusy('btn-fetch-assets', false, '获取固件列表');
			});
	}

	function escapeRegex(s) {
		return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	}

	function applyAsset() {
		var name = $('cfg-asset').value;
		if (!name) return;
		$('cfg-pattern').value = '^' + escapeRegex(name) + '$';
		hint('config-hint', '✅ 已按所选文件生成匹配规则(文件名固定匹配;如文件名会变化可在高级配置放宽)', 'ok');
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
			github_token: $('cfg-token').value
		};
		[['dl_workers', 'cfg-workers'], ['dl_chunk_mb', 'cfg-chunk'],
		 ['dl_stall_sec', 'cfg-stall'], ['dl_retries', 'cfg-retries'],
		 ['min_size_mb', 'cfg-minsize']].forEach(function (f) {
			var v = parseInt($(f[1]).value, 10);
			if (!isNaN(v)) cfg[f[0]] = v;
		});
		ubus('set_config', cfg).then(function (r) {
			if (r && r.ok) {
				hint('config-hint', '✅ 配置已保存', 'ok');
				updateSummary();
			} else {
				hint('config-hint', '❌ ' + (r && r.error ? r.error : '保存失败'), 'err');
			}
		}).catch(function (e) {
			hint('config-hint', '❌ ' + e.message, 'err');
		}).finally(function () {
			setBusy('btn-save-config', false, '保存配置');
		});
	}

	function renderSysinfo(info) {
		setText('cur-ver', (info.id || 'OpenWrt') + ' ' + (info.release || '') +
			(info.revision ? ' (' + info.revision + ')' : ''));
	}

	/* ---------------- log ---------------- */

	function refreshLog() {
		ubus('get_log').then(function (r) {
			$('log-view').textContent = (r && r.log) ? r.log : '日志为空';
			$('log-view').scrollTop = $('log-view').scrollHeight;
		}).catch(function (e) {
			$('log-view').textContent = '日志读取失败: ' + e.message;
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
		$('btn-apply-asset').addEventListener('click', applyAsset);
		$('btn-save-config').addEventListener('click', saveConfig);
		$('btn-refresh-log').addEventListener('click', refreshLog);
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
			hint('config-hint', '❌ 读取配置失败: ' + e.message, 'err');
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
