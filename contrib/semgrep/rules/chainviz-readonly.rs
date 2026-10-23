// Test cases for chainviz-readonly.yaml (semgrep --test).
async fn reads(rpc: &Rpc) {
    // ok: chainviz.readonly.writer-rpc
    rpc.call("yed_getinfo", json!([])).await;
    // ok: chainviz.readonly.writer-rpc
    rpc.call("yed_listvaults", json!([])).await;
    // ok: chainviz.readonly.writer-rpc
    rpc.call("yed_validaterawtransaction", json!([hex])).await;
    // ok: chainviz.readonly.writer-rpc
    rpc.call("vault_decodescript", json!([hex])).await;
    // ok: chainviz.readonly.writer-rpc
    rpc.call("set_getinfo", json!([id])).await;
    // ok: chainviz.readonly.writer-rpc
    let kind = "generated";
}

async fn writes(rpc: &Rpc) {
    // ruleid: chainviz.readonly.writer-rpc
    rpc.call("sendrawtransaction", json!([hex])).await;
    // ruleid: chainviz.readonly.writer-rpc
    rpc.call("yed_mint", json!([])).await;
    // ruleid: chainviz.readonly.writer-rpc
    rpc.call("yed_somefuturemutator", json!([])).await;
    // ruleid: chainviz.readonly.writer-rpc
    let m = "set_sendact";
    // ruleid: chainviz.readonly.writer-rpc
    rpc.call("vault_buildunlock", json!([])).await;
}
