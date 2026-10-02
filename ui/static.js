// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

// Static mode: installed by `chain-viz --export <dir>` ahead of app.js, after ui/data.js has set
// window.CHAIN_VIZ_STATIC = {snapshot, events, health, exported}. Answers the app's /api/*
// fetches from that object (so the page works from a plain static host and, in browsers that
// allow module scripts there, from file://) and stubs WebSocket so nothing tries to connect.
// The served UI never loads this file.
(() => {
  const data = window.CHAIN_VIZ_STATIC;
  if (!data) return;
  const reply = (v) => Promise.resolve(new Response(JSON.stringify(v), { status: 200, headers: { 'content-type': 'application/json' } }));
  const realFetch = window.fetch ? window.fetch.bind(window) : undefined;
  window.fetch = (input, init) => {
    const url = typeof input === 'string' ? input : input.url;
    const path = url.replace(/^[a-z]+:\/\/[^/]+/i, '');
    if (path.startsWith('/api/events')) {
      const since = Number(new URL(path, 'http://static.invalid').searchParams.get('since') || 0);
      return reply((data.events || []).filter((e) => e.seq > since));
    }
    if (path === '/api/snapshot') return reply(data.snapshot);
    if (path === '/api/health') return reply(data.health);
    if (path.startsWith('/api/')) return reply(null);
    return realFetch ? realFetch(input, init) : Promise.reject(new Error('no fetch'));
  };
  window.WebSocket = class { constructor(url) { this.url = url; } close() {} send() {} };
})();
