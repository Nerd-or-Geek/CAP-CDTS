/* Minimal JS helpers for the CAP 2.0 HTML prototypes.
   - Optional live updates from /ws (Axum backend)
   - Small utilities for navigation highlighting, formatting, and demo data
*/

(function () {
    "use strict";

    function qs(sel, root) {
        return (root || document).querySelector(sel);
    }

    function qsa(sel, root) {
        return Array.from((root || document).querySelectorAll(sel));
    }

    function setText(id, text) {
        var el = document.getElementById(id);
        if (!el) return;
        el.textContent = text;
    }

    function setDotState(id, state) {
        var el = document.getElementById(id);
        if (!el) return;
        el.classList.remove("ok", "bad", "neutral");
        if (state === "ok") el.classList.add("ok");
        else if (state === "bad") el.classList.add("bad");
        else el.classList.add("neutral");
    }

    function fmtTime(iso) {
        try {
            var d = new Date(iso);
            if (Number.isNaN(d.getTime())) return iso;
            return d.toLocaleString();
        } catch (_e) {
            return iso;
        }
    }

    function markActiveNav() {
        var path = location.pathname.split("/").pop() || "index.html";
        qsa(".nav a").forEach(function (a) {
            var href = a.getAttribute("href") || "";
            var file = href.split("/").pop();
            if (!file) return;
            if (file === path) {
                a.setAttribute("aria-current", "page");
            } else {
                a.removeAttribute("aria-current");
            }
        });
    }

    function defaultWsUrl() {
        var proto = location.protocol === "https:" ? "wss:" : "ws:";
        // When opened from file://, location.host is empty. Default to the typical dev server.
        var host = location.host || "localhost:8080";
        return proto + "//" + host + "/ws";
    }

    function defaultApiBase() {
        if (location.protocol === "http:" || location.protocol === "https:") {
            return location.origin;
        }
        // Allow opening these HTML files directly while the backend runs separately.
        return "http://localhost:8080";
    }

    async function apiJson(method, path, body) {
        var base = defaultApiBase();
        var url = path;
        if (typeof path === "string" && !path.startsWith("http://") && !path.startsWith("https://")) {
            url = base + path;
        }

        var init = {
            method: method,
            headers: {
                "Accept": "application/json",
            },
        };

        if (body !== undefined) {
            init.headers["Content-Type"] = "application/json";
            init.body = JSON.stringify(body);
        }

        try {
            var resp = await fetch(url, init);
            var text = await resp.text();
            var data = null;
            try {
                data = text ? JSON.parse(text) : null;
            } catch (_e) {
                data = text;
            }
            return { ok: resp.ok, status: resp.status, data: data };
        } catch (e) {
            return { ok: false, status: 0, data: null, error: (e && e.message) ? e.message : String(e) };
        }
    }

    function apiGet(path) {
        return apiJson("GET", path);
    }

    function apiPost(path, body) {
        return apiJson("POST", path, body);
    }

    function connectLiveState(onState) {
        var url = defaultWsUrl();
        var ws;
        try {
            ws = new WebSocket(url);
        } catch (_e) {
            return { close: function () {} };
        }

        ws.onopen = function () {
            setDotState("wsDot", "ok");
            setText("wsText", "Connected");
        };

        ws.onclose = function () {
            setDotState("wsDot", "bad");
            setText("wsText", "Disconnected");
        };

        ws.onerror = function () {
            setDotState("wsDot", "bad");
            setText("wsText", "Error");
        };

        ws.onmessage = function (evt) {
            try {
                var state = JSON.parse(evt.data);
                if (typeof onState === "function") onState(state);
            } catch (_e) {
                // Ignore
            }
        };

        return {
            close: function () {
                try { ws.close(); } catch (_e) {}
            },
        };
    }

    function renderAuthSummary(state) {
        if (!state || !state.auth) return "—";
        var stage = state.auth.stage || "Unknown";
        var user = state.auth.user && state.auth.user.username ? state.auth.user.username : null;
        if (user) return stage + " (" + user + ")";
        return stage;
    }

    function renderUserRole(state) {
        var lvl = state && state.auth && state.auth.user ? state.auth.user.level : null;
        if (lvl === 1) return "Admin";
        if (lvl === 0) return "User";
        return "—";
    }

    function applyLiveStateToCommonBadges(state) {
        setText("authText", renderAuthSummary(state));
        setText("roleText", renderUserRole(state));
        if (state && state.last_update_utc) {
            setText("updatedText", fmtTime(state.last_update_utc));
        }
    }

    // Demo data (used when backend isn't running)
    function demoReports() {
        return [
            { num: 123456, created_at_utc: new Date().toISOString(), opened_by: "MOCK-ADMIN", opened_by_level: 1, closed_by: null, closed_at_utc: null, closing_comments: null },
            { num: 123455, created_at_utc: new Date(Date.now() - 3600_000).toISOString(), opened_by: "jdoe", opened_by_level: 0, closed_by: "MOCK-ADMIN", closed_at_utc: new Date(Date.now() - 1800_000).toISOString(), closing_comments: "Reviewed and archived." },
        ];
    }

    function loadSessionReports() {
        try {
            var raw = sessionStorage.getItem("cap_ui_reports_v2");
            if (!raw) return [];
            var parsed = JSON.parse(raw);
            return Array.isArray(parsed) ? parsed : [];
        } catch (_e) {
            return [];
        }
    }

    function addSessionReport(report) {
        try {
            var arr = loadSessionReports();
            arr.unshift(report);
            // Prevent unbounded growth.
            sessionStorage.setItem("cap_ui_reports_v2", JSON.stringify(arr.slice(0, 100)));
        } catch (_e) {
            // ignore
        }
    }

    function demoUsers() {
        return [
            { username: "MOCK-ADMIN", rfid_uid: "DE AD BE EF", level: 1 },
            { username: "jdoe", rfid_uid: "01 23 45 67", level: 0 },
        ];
    }

    // Public API
    window.CAPUI = {
        markActiveNav: markActiveNav,
        connectLiveState: connectLiveState,
        applyLiveStateToCommonBadges: applyLiveStateToCommonBadges,
        apiGet: apiGet,
        apiPost: apiPost,
        defaultApiBase: defaultApiBase,
        demoReports: demoReports,
        loadSessionReports: loadSessionReports,
        addSessionReport: addSessionReport,
        demoUsers: demoUsers,
    };
})();
