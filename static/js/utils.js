export function escHtml(s) {
  s = String(s ?? '');
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

export function truncateStr(s, max) {
  return s.length > max ? s.slice(0, max) + '…' : s;
}

export function formatTime(d) {
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

export function timeAgo(ts) {
  const diff = Date.now() - ts * 1000;
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return '刚刚';
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}小时前`;
  const day = Math.floor(hr / 24);
  if (day < 30) return `${day}天前`;
  return new Date(ts * 1000).toLocaleDateString('zh-CN');
}

export function formatToolDuration(durationMs) {
  if (durationMs == null) return '';
  if (durationMs < 1000) {
    return `${Math.max(1, Math.round(durationMs))}ms`;
  }
  return `${(durationMs / 1000).toFixed(durationMs < 10000 ? 1 : 0)}s`;
}

export function fallbackCopy(text) {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.cssText = 'position:fixed;left:-9999px';
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  document.body.removeChild(ta);
}

export function copyText(text) {
  const value = String(text ?? '');
  if (!value) return Promise.resolve(false);
  if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
    return navigator.clipboard.writeText(value)
      .then(() => true)
      .catch(() => {
        fallbackCopy(value);
        return true;
      });
  }
  fallbackCopy(value);
  return Promise.resolve(true);
}

export function formatDetailText(text) {
  const raw = String(text ?? '');
  if (!raw) return '';
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

export function inlinePreview(text, max = 100) {
  return truncateStr(String(text ?? '').replace(/\s+/g, ' ').trim(), max);
}

export function pulseFocus(el) {
  if (!el) return;
  el.classList.remove('focus-flash');
  void el.offsetWidth;
  el.classList.add('focus-flash');
  if (el._focusFlashTimer) window.clearTimeout(el._focusFlashTimer);
  el._focusFlashTimer = window.setTimeout(() => {
    el.classList.remove('focus-flash');
  }, 1100);
}

export async function copyButtonText(button, text, idleLabel) {
  const payload = String(text ?? '').trim();
  if (!button || !payload) return;

  const original = button.dataset.idleLabel || idleLabel || button.textContent || '复制摘要';
  button.dataset.idleLabel = original;
  button.disabled = true;
  button.textContent = '复制中…';

  try {
    await copyText(payload);
    button.textContent = '已复制';
  } catch {
    button.textContent = '复制失败';
  }

  if (button._resetLabelTimer) window.clearTimeout(button._resetLabelTimer);
  button._resetLabelTimer = window.setTimeout(() => {
    button.disabled = false;
    button.textContent = original;
  }, 1200);
}

export function afterNextPaint(callback) {
  requestAnimationFrame(() => requestAnimationFrame(callback));
}

export function scheduleBackgroundTask(callback, timeout = 180) {
  if (typeof requestIdleCallback === 'function') {
    return requestIdleCallback(callback, { timeout });
  }
  return setTimeout(callback, 16);
}

export function cancelBackgroundTask(handle) {
  if (!handle) return;
  if (typeof cancelIdleCallback === 'function') {
    cancelIdleCallback(handle);
  } else {
    clearTimeout(handle);
  }
}

export function isAsciiDigit(ch) {
  return ch >= '0' && ch <= '9';
}

export function reactPhaseLabel(phase) {
  return {
    analyze: 'Analyze',
    act: 'Act',
    observe: 'Observe'
  }[phase] || phase || 'Analyze';
}

export function formatTokenCount(n) {
  if (n == null || n === 0) return '0';
  if (n < 1000) return String(n);
  if (n < 10000) return (n / 1000).toFixed(1) + 'K';
  if (n < 1000000) return Math.round(n / 1000) + 'K';
  return (n / 1000000).toFixed(1) + 'M';
}

export function hideWelcome() {
  const w = document.getElementById('welcome');
  if (w) w.remove();
}

export function canSendWhileBusy(cmd) {
  return /^\/(tool|reasoning|stop)\b/i.test(cmd);
}
