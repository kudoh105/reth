// peer.js
const NODES = [
    { name: "V1", url: "http://127.0.0.1:8545", ip: "172.20.0.11" },
    { name: "V2", url: "http://127.0.0.1:8546", ip: "172.20.0.12" },
    { name: "V3", url: "http://127.0.0.1:8547", ip: "172.20.0.13" },
    { name: "V4", url: "http://127.0.0.1:8548", ip: "172.20.0.14" }
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
    console.log("🔍 1. 각 노드의 고유 enode 주소를 추출합니다...");
    const enodes = [];

    for (const node of NODES) {
        const info = await rpc(node.url, "admin_nodeInfo");
        // 도커 내부 IP(127.0.0.1)로 찍힌 것을, docker-compose에 설정한 고정 IP로 치환!
        const fixedEnode = info.enode.replace(/@[^:]+:/, `@${node.ip}:`);
        enodes.push({ name: node.name, enode: fixedEnode });
        console.log(`✅ ${node.name} enode: ${fixedEnode.substring(0, 30)}...`);
    }

    console.log("\n🔗 2. P2P 네트워크(Mempool) 교차 연결을 시작합니다...");
    
    for (let i = 0; i < NODES.length; i++) {
        for (let j = 0; j < enodes.length; j++) {
            if (i !== j) { // 자기 자신이 아닐 때만 연결
                await rpc(NODES[i].url, "admin_addPeer", [enodes[j].enode]);
                console.log(`   [${NODES[i].name}] ----연결----> [${enodes[j].name}]`);
            }
        }
    }

    console.log("\n🎉 완벽한 P2P 클러스터가 구성되었습니다! 이제 V1에만 트랜잭션을 쏴도 됩니다.");
}

connectP2PNetwork().catch(console.error);