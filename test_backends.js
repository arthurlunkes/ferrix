const http = require("http");

function createBackend(name, port) {
  const server = http.createServer((req, res) => {
    if (req.url === "/health") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ status: "ok", server: name }));
      return;
    }

    const forwardedFor = req.headers["x-forwarded-for"];
    const realIp = req.headers["x-real-ip"];

    res.writeHead(200, {
      "Content-Type": "application/json",
      "X-Server-Node": name,
    });
    res.end(
      JSON.stringify({
        message: `Hello from ${name}!`,
        url: req.url,
        method: req.method,
        headers: {
          "x-forwarded-for": forwardedFor,
          "x-real-ip": realIp,
          host: req.headers.host,
        },
        time: new Date().toISOString(),
      }),
    );
  });

  server.listen(port, "127.0.0.1", () => {
    console.log(`[${name}] listening on http://127.0.0.1:${port}`);
  });

  return server;
}

const b1 = createBackend("Backend-1", 9001);
const b2 = createBackend("Backend-2", 9002);

process.on("SIGINT", () => {
  b1.close();
  b2.close();
  process.exit();
});
