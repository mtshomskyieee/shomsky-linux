(function () {
  var term = new Terminal({
    cursorBlink: true,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    fontSize: 14,
    theme: { background: "#0d1117" }
  });
  var fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(document.getElementById("term"));
  fit.fit();

  var proto = location.protocol === "https:" ? "wss:" : "ws:";
  var ws = new WebSocket(proto + "//" + location.host + "/ws");
  ws.binaryType = "arraybuffer";

  function sendResize() {
    if (ws.readyState !== WebSocket.OPEN) return;
    fit.fit();
    ws.send(JSON.stringify({ cols: term.cols, rows: term.rows }));
  }

  ws.onopen = function () {
    sendResize();
    term.focus();
  };
  ws.onmessage = function (ev) {
    if (typeof ev.data === "string") {
      term.write(ev.data);
      return;
    }
    term.write(new Uint8Array(ev.data));
  };
  ws.onclose = function () {
    term.write("\r\n[emerge] disconnected\r\n");
  };
  term.onData(function (data) {
    if (ws.readyState !== WebSocket.OPEN) return;
    ws.send(new TextEncoder().encode(data));
  });
  window.addEventListener("resize", sendResize);
})();
