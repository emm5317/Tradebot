/* ═══════════════════════════════════════════════════════════════════
   TRADEBOT TERMINAL — Shared JavaScript
   ═══════════════════════════════════════════════════════════════════ */

const T = window.Terminal = {};

// ── Fetch helper ────────────────────────────────────────────────────
T.fetchJSON = function(url) {
    return fetch(url).then(function(r) { return r.json(); }).catch(function() { return null; });
};

// ── Formatting ──────────────────────────────────────────────────────
T.ageText = function(date) {
    const s = Math.floor((Date.now() - date) / 1000);
    if (s < 0) return 'now';
    if (s < 60) return s + 's';
    if (s < 3600) return Math.floor(s / 60) + 'm';
    if (s < 86400) return Math.floor(s / 3600) + 'h';
    return Math.floor(s / 86400) + 'd';
};

T.ageClass = function(date) {
    const s = (Date.now() - date) / 1000;
    if (s < 60) return 't-age-fresh';
    if (s < 300) return 't-age-recent';
    if (s < 1800) return 't-age-old';
    return 't-age-stale';
};

T.formatPnl = function(cents) {
    var d = cents / 100;
    var s = (d >= 0 ? '+' : '') + '$' + Math.abs(d).toFixed(2);
    if (d < 0) s = '-$' + Math.abs(d).toFixed(2);
    return s;
};

T.pnlClass = function(cents) {
    if (cents > 0) return 't-pnl-pos';
    if (cents < 0) return 't-pnl-neg';
    return 't-pnl-zero';
};

T.formatPct = function(decimal) {
    return (decimal * 100).toFixed(1) + '%';
};

T.rejClass = function(reason) {
    if (!reason) return 't-rej-generic';
    if (reason.includes('edge')) return 't-rej-edge';
    if (reason.includes('position')) return 't-rej-position';
    if (reason.includes('risk')) return 't-rej-risk';
    return 't-rej-generic';
};

// ── Clock ───────────────────────────────────────────────────────────
T.startClock = function() {
    function tick() {
        var el = document.getElementById('t-clock');
        if (!el) return;
        var now = new Date();
        var h = String(now.getUTCHours()).padStart(2, '0');
        var m = String(now.getUTCMinutes()).padStart(2, '0');
        var s = String(now.getUTCSeconds()).padStart(2, '0');
        el.textContent = h + ':' + m + ':' + s + ' UTC';
    }
    tick();
    setInterval(tick, 1000);
};

// ── Age Ticker ──────────────────────────────────────────────────────
T.tickAges = function() {
    document.querySelectorAll('[data-ts]').forEach(function(el) {
        var ts = parseInt(el.dataset.ts);
        var d = new Date(ts);
        el.textContent = T.ageText(d);
        // Preserve non-age classes, update age class
        var classes = el.className.split(' ').filter(function(c) {
            return !c.startsWith('t-age-');
        });
        classes.push(T.ageClass(d));
        el.className = classes.join(' ');
    });
};

// ── Status Bar ──────────────────────────────────────────────────────
T.updateStatus = function(data) {
    if (!data) return;

    // BTC price
    var btcEl = document.getElementById('t-btc-price');
    if (btcEl && data.btc_price) {
        btcEl.textContent = '$' + Number(data.btc_price).toLocaleString('en-US', {
            minimumFractionDigits: 0, maximumFractionDigits: 0
        });
    }

    // Daily P&L
    var pnlEl = document.getElementById('t-daily-pnl');
    if (pnlEl && data.daily_pnl_cents !== undefined) {
        pnlEl.textContent = T.formatPnl(data.daily_pnl_cents);
        pnlEl.className = 't-topbar-value ' + T.pnlClass(data.daily_pnl_cents);
    }

    // Feed dots
    var dotsEl = document.getElementById('t-feed-dots');
    if (dotsEl && data.feeds) {
        var feeds = ['coinbase', 'binance_spot', 'binance_futures', 'deribit', 'kalshi_ws'];
        var html = '';
        feeds.forEach(function(name) {
            var f = data.feeds[name];
            var cls = 'stale';
            if (f) {
                if (f.score >= 0.9) cls = 'ok';
                else if (f.score >= 0.5) cls = 'warn';
                else if (f.score > 0) cls = 'err';
            }
            html += '<div class="t-feed-dot ' + cls + '" title="' + name + (f ? ' (' + f.score.toFixed(2) + ')' : '') + '"></div>';
        });
        dotsEl.innerHTML = html;
    }

    // Status bar items
    var posEl = document.getElementById('t-positions-count');
    if (posEl && data.positions_count !== undefined) posEl.textContent = data.positions_count;

    var sigRateEl = document.getElementById('t-signal-rate');
    if (sigRateEl && data.signal_rate_1h !== undefined) sigRateEl.textContent = data.signal_rate_1h + '/hr';

    var brierEl = document.getElementById('t-brier');
    if (brierEl && data.brier_score !== undefined) brierEl.textContent = data.brier_score !== null ? data.brier_score.toFixed(3) : '—';

    var latEl = document.getElementById('t-latency');
    if (latEl && data.avg_latency_ms !== undefined) latEl.textContent = data.avg_latency_ms !== null ? data.avg_latency_ms + 'ms' : '—';

    var modeEl = document.getElementById('t-mode');
    if (modeEl && data.paper_mode !== undefined) {
        modeEl.textContent = data.paper_mode ? 'PAPER' : 'LIVE';
        modeEl.className = data.paper_mode ? 't-mode-paper' : 't-mode-live';
    }
};

// ── SSE Connection ──────────────────────────────────────────────────
T.sse = null;
T.sseHandlers = {};

T.onSSE = function(event, handler) {
    if (!T.sseHandlers[event]) T.sseHandlers[event] = [];
    T.sseHandlers[event].push(handler);
};

T.connectSSE = function() {
    if (T.sse) {
        try { T.sse.close(); } catch(e) {}
    }

    T.sse = new EventSource('/api/events');

    T.sse.addEventListener('model_state', function(e) {
        var data = JSON.parse(e.data);
        (T.sseHandlers['model_state'] || []).forEach(function(h) { h(data); });
    });

    T.sse.addEventListener('signal', function(e) {
        var data = JSON.parse(e.data);
        (T.sseHandlers['signal'] || []).forEach(function(h) { h(data); });
    });

    T.sse.addEventListener('system_status', function(e) {
        var data = JSON.parse(e.data);
        T.updateStatus(data);
    });

    T.sse.onerror = function() {
        // Reconnect after 5s
        setTimeout(T.connectSSE, 5000);
    };
};

// ── Keyboard Navigation ─────────────────────────────────────────────
T.initKeyboard = function() {
    var tabMap = {
        '1': '/',
        '2': '/signals',
        '3': '/execution',
        '4': '/analytics',
        '5': '/risk',
        '6': '/weather'
    };

    document.addEventListener('keydown', function(e) {
        var tag = (document.activeElement || {}).tagName;
        if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;

        if (tabMap[e.key] && !e.ctrlKey && !e.metaKey && !e.altKey) {
            e.preventDefault();
            window.location.href = tabMap[e.key];
        }

        if (e.key.toLowerCase() === 'r' && !e.ctrlKey && !e.metaKey) {
            e.preventDefault();
            if (typeof T.refreshPage === 'function') T.refreshPage();
        }
    });
};

// ── Sparkline ───────────────────────────────────────────────────────
T.sparkline = function(canvas, data, opts) {
    if (!canvas || !data || !data.length) return;
    opts = opts || {};

    var ctx = canvas.getContext('2d');
    var w = canvas.width = opts.width || canvas.offsetWidth || 120;
    var h = canvas.height = opts.height || canvas.offsetHeight || 24;
    var color = opts.color || '#00c853';
    var negColor = opts.negColor || '#ff1744';
    var fill = opts.fill !== false;

    ctx.clearRect(0, 0, w, h);

    var min = Math.min.apply(null, data);
    var max = Math.max.apply(null, data);
    if (min === max) { min -= 1; max += 1; }

    var padding = 1;
    var range = max - min;
    var step = (w - padding * 2) / (data.length - 1);

    function y(val) {
        return h - padding - ((val - min) / range) * (h - padding * 2);
    }

    // Fill
    if (fill) {
        ctx.beginPath();
        ctx.moveTo(padding, y(data[0]));
        for (var i = 1; i < data.length; i++) {
            ctx.lineTo(padding + i * step, y(data[i]));
        }
        ctx.lineTo(padding + (data.length - 1) * step, h);
        ctx.lineTo(padding, h);
        ctx.closePath();

        var zeroY = y(0);
        var grad = ctx.createLinearGradient(0, 0, 0, h);
        grad.addColorStop(0, color + '20');
        grad.addColorStop(1, color + '05');
        ctx.fillStyle = grad;
        ctx.fill();
    }

    // Line
    ctx.beginPath();
    ctx.moveTo(padding, y(data[0]));
    for (var j = 1; j < data.length; j++) {
        ctx.lineTo(padding + j * step, y(data[j]));
    }
    ctx.strokeStyle = data[data.length - 1] >= 0 ? color : negColor;
    ctx.lineWidth = 1.5;
    ctx.stroke();

    // End dot
    var lastX = padding + (data.length - 1) * step;
    var lastY = y(data[data.length - 1]);
    ctx.beginPath();
    ctx.arc(lastX, lastY, 2, 0, Math.PI * 2);
    ctx.fillStyle = data[data.length - 1] >= 0 ? color : negColor;
    ctx.fill();
};

// ── Sort Helper ─────────────────────────────────────────────────────
T.sortState = {};

T.bindSort = function(tableEl, key, renderFn) {
    if (!T.sortState[key]) T.sortState[key] = { col: 0, dir: -1 };
    tableEl.querySelectorAll('th[data-col]').forEach(function(th) {
        th.addEventListener('click', function() {
            var col = parseInt(th.dataset.col);
            var st = T.sortState[key];
            if (st.col === col) st.dir *= -1;
            else { st.col = col; st.dir = 1; }
            renderFn();
        });
    });
};

T.applySort = function(arr, key, cols) {
    var st = T.sortState[key];
    if (!st) return arr;
    return arr.slice().sort(function(a, b) {
        var av = cols[st.col].fn(a);
        var bv = cols[st.col].fn(b);
        return av < bv ? -st.dir : av > bv ? st.dir : 0;
    });
};

// ── Meta update flash ───────────────────────────────────────────────
T.touchMeta = function(id) {
    var el = document.getElementById('meta-' + id);
    if (!el) return;
    el.textContent = 'updated';
    el.classList.add('fresh');
    setTimeout(function() { el.classList.remove('fresh'); }, 2000);
};

// ── Init ────────────────────────────────────────────────────────────
T.init = function() {
    T.startClock();
    T.initKeyboard();
    T.connectSSE();
    setInterval(T.tickAges, 10000);

    // Fetch initial status
    T.fetchJSON('/api/system-status').then(T.updateStatus);
    // Poll status as backup to SSE
    setInterval(function() {
        T.fetchJSON('/api/system-status').then(T.updateStatus);
    }, 10000);
};

// Auto-init on DOMContentLoaded
document.addEventListener('DOMContentLoaded', T.init);
