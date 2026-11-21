// Standalone site entry. The post-build VM compiles this file as one unit.
// It uses no imports or exports, so the browser can run its VM replacement.
if ('serviceWorker' in navigator) {
	window.addEventListener('load', function () {
		navigator.serviceWorker.register('/sw.js').catch(function () {});
	});
}

const timeEl = document.getElementById('buildTime');
const agoEl = document.getElementById('buildAgo');

if (timeEl && agoEl) {
	const built = Date.parse(timeEl.getAttribute('datetime') || '');
	if (!Number.isNaN(built)) {
		function ago(ms) {
			const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
			if (s < 45) return 'just now';
			const m = Math.round(s / 60);
			if (s < 90) return '1 minute ago';
			if (m < 45) return m + ' minutes ago';
			const h = Math.round(m / 60);
			if (m < 90) return '1 hour ago';
			if (h < 24) return h + ' hours ago';
			const d = Math.round(h / 24);
			if (h < 36) return '1 day ago';
			if (d < 30) return d + ' days ago';
			const mo = Math.round(d / 30);
			if (d < 45) return '1 month ago';
			if (mo < 12) return mo + ' months ago';
			const y = Math.round(mo / 12);
			return y <= 1 ? '1 year ago' : y + ' years ago';
		}

		function render() {
			agoEl.textContent = '(' + ago(built) + ')';
		}

		render();
		window.setInterval(render, 30000);
	}
}
