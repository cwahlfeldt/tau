(function () {
  var url = './__tauri/reload-token';
  var last = null;
  function poll() {
    fetch(url, { cache: 'no-store' })
      .then(function (r) { return r.ok ? r.text() : null; })
      .then(function (t) {
        if (t === null) return;
        if (last !== null && t !== last) {
          window.location.reload();
          return;
        }
        last = t;
      })
      .catch(function () {})
      .finally(function () { setTimeout(poll, 300); });
  }
  poll();
})();
