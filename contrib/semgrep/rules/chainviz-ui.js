// Test cases for chainviz-ui.yaml (semgrep --test).
function ok(node, memo) {
  // ok: chainviz.ui.html-sink
  node.textContent = memo;
  // ok: chainviz.ui.html-sink
  node.innerHTML = "";
  // ok: chainviz.ui.html-sink
  node.innerHTML = '';
  // ok: chainviz.ui.code-from-string
  setTimeout(() => refresh(), 1000);
}

function bad(node, memo, txid) {
  // ruleid: chainviz.ui.html-sink
  node.innerHTML = memo;
  // ruleid: chainviz.ui.html-sink
  node.innerHTML = `<b>${txid}</b>`;
  // ruleid: chainviz.ui.html-sink
  node.insertAdjacentHTML("beforeend", "<i>" + memo + "</i>");
  // ruleid: chainviz.ui.html-sink
  document.write(memo);
  // ruleid: chainviz.ui.code-from-string
  eval(memo);
  // ruleid: chainviz.ui.code-from-string
  new Function("return " + memo);
}
