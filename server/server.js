const WebSocket = require('ws');
const http = require('http');
const fs = require('fs');
const path = require('path');

const PORT = 3000;

// Create HTTP server to serve the client
const server = http.createServer((req, res) => {
  if (req.url === '/' || req.url === '/index.html') {
    const clientPath = path.join(__dirname, '..', 'client', 'index.html');
    fs.readFile(clientPath, (err, data) => {
      if (err) {
        res.writeHead(500);
        res.end('Error loading client');
        return;
      }
      res.writeHead(200, { 'Content-Type': 'text/html' });
      res.end(data);
    });
  } else {
    res.writeHead(404);
    res.end('Not found');
  }
});

// Create WebSocket server
const wss = new WebSocket.Server({ server });

let waitingClient = null;
let clients = [];

wss.on('connection', (ws) => {
  console.log('New client connected');
  
  ws.on('message', (message) => {
    try {
      const data = JSON.parse(message);
      console.log('Received:', data.type);
      
      switch (data.type) {
        case 'join':
          handleJoin(ws);
          break;
        case 'offer':
        case 'answer':
        case 'ice-candidate':
          // Forward signaling messages to the peer
          forwardToPeer(ws, data);
          break;
      }
    } catch (error) {
      console.error('Error handling message:', error);
    }
  });
  
  ws.on('close', () => {
    console.log('Client disconnected');
    handleDisconnect(ws);
  });
});

function handleJoin(ws) {
  if (waitingClient === null) {
    // First client - wait for another
    waitingClient = ws;
    ws.peerId = null;
    console.log('First client waiting for peer...');
    ws.send(JSON.stringify({ type: 'waiting' }));
  } else {
    // Second client - pair them up
    const peer1 = waitingClient;
    const peer2 = ws;
    
    // Link the peers
    peer1.peerId = peer2;
    peer2.peerId = peer1;
    
    clients.push({ peer1, peer2 });
    waitingClient = null;
    
    console.log('Two clients paired! Starting WebRTC handshake...');
    
    // Tell first client to initiate the offer
    peer1.send(JSON.stringify({ type: 'ready', initiator: true }));
    peer2.send(JSON.stringify({ type: 'ready', initiator: false }));
  }
}

function forwardToPeer(ws, data) {
  if (ws.peerId && ws.peerId.readyState === WebSocket.OPEN) {
    ws.peerId.send(JSON.stringify(data));
  }
}

function handleDisconnect(ws) {
  // If disconnected client was waiting, clear it
  if (waitingClient === ws) {
    waitingClient = null;
  }
  
  // Notify peer if connected
  if (ws.peerId && ws.peerId.readyState === WebSocket.OPEN) {
    ws.peerId.send(JSON.stringify({ type: 'peer-disconnected' }));
    ws.peerId.peerId = null;
  }
  
  // Remove from clients array
  clients = clients.filter(pair => pair.peer1 !== ws && pair.peer2 !== ws);
}

server.listen(PORT, () => {
  console.log(`Server running on http://localhost:${PORT}`);
  console.log(`WebSocket server ready on ws://localhost:${PORT}`);
});
