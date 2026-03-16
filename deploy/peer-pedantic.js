// peer-pedantic.js — devp2p(mempool) peer connect for pedantic test network
const NODES = [
    { name: "V1", url: "http://127.0.0.1:9545", ip: "172.21.0.11" },
    { name: "V2", url: "http://127.0.0.1:9546", ip: "172.21.0.12" },
    { name: "V3", url: "http://127.0.0.1:9547", ip: "172.21.0.13" },
    { name: "V4", url: "http://127.0.0.1:9548", ip: "172.21.0.14" }
];

async function rpc(url, method, params = []) {
    const response = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params })
    });
    const data = await response.json();
    return data.result;
}

async function connectP2PNetwork() {
    console.log("🔍 1. 각 노드의 enode 주소를 추출합니다...");
    const enodes = [];

    for (const node of NODES) {
        const info = await rpc(node.url, "admin_nodeInfo");
        const fixedEnode = info.enode.replace(/@[^:]+:/, `@${node.ip}:`);
        enodes.push({ name: node.name, enode: fixedEnode });
        console.log(`✅ ${node.name} enode: ${fixedEnode.substring(0, 60)}...`);
    }

    console.log("\n🔗 2. devp2p 교차 연결을 시작합니다...");

    for (let i = 0; i < NODES.length; i++) {
        for (let j = 0; j < enodes.length; j++) {
            if (i !== j) {
                await rpc(NODES[i].url, "admin_addPeer", [enodes[j].enode]);
                console.log(`   [${NODES[i].name}] → [${enodes[j].name}]`);
            }
        }
    }

    console.log("\n🎉 pedantic P2P 클러스터 연결 완료!");
}

connectP2PNetwork().catch(console.error);
