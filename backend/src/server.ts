import express, { Request, Response, NextFunction } from "express";
import cors from "cors";
import path from "path";
import fs from "fs";
import http from "http";

const app = express();
const PORT = process.env.PORT || 8080;
const CORE_URL = process.env.FERRIX_CORE_URL || "http://127.0.0.1:8081";
const ADMIN_TOKEN = process.env.FERRIX_ADMIN_TOKEN || "";

app.use(cors());
app.use(express.json({ limit: "10mb" }));

function getRequestAuth(req: Request): string | undefined {
  const auth = req.headers["authorization"];
  if (auth) return auth;
  const tokenHeader = req.headers["x-ferrix-token"];
  if (tokenHeader && typeof tokenHeader === "string") {
    return `Bearer ${tokenHeader}`;
  }
  const tokenQuery = req.query.token;
  if (tokenQuery && typeof tokenQuery === "string") {
    return `Bearer ${tokenQuery}`;
  }
  if (ADMIN_TOKEN) {
    return `Bearer ${ADMIN_TOKEN}`;
  }
  return undefined;
}

async function proxyToCore(
  req: Request,
  res: Response,
  targetPath: string,
  method: string = req.method,
) {
  try {
    const coreUrl = new URL(targetPath, CORE_URL);

    for (const [k, v] of Object.entries(req.query)) {
      if (v !== undefined) {
        coreUrl.searchParams.append(k, String(v));
      }
    }

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      Accept: "application/json",
    };

    const auth = getRequestAuth(req);
    if (auth) {
      headers["Authorization"] = auth;
    }

    const options: RequestInit = {
      method,
      headers,
    };

    if (
      ["POST", "PUT", "PATCH"].includes(method) &&
      req.body &&
      Object.keys(req.body).length > 0
    ) {
      options.body = JSON.stringify(req.body);
    }

    const coreRes = await fetch(coreUrl.toString(), options);
    const contentType = coreRes.headers.get("content-type") || "";

    res.status(coreRes.status);
    if (contentType.includes("application/json")) {
      const data = await coreRes.json();
      res.json(data);
    } else {
      const text = await coreRes.text();
      res.send(text);
    }
  } catch (err: any) {
    console.error(
      `[Ferrix Backend] Error connecting to Core at ${CORE_URL}${targetPath}:`,
      err.message,
    );
    res.status(502).json({
      error: "Core Unreachable",
      message: `Could not connect to Ferrix Core at ${CORE_URL}. Check if the process is running.`,
      details: err.message,
    });
  }
}

app.get("/api/status", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/status");
});

app.get("/api/metrics", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/metrics");
});

app.get("/api/logs", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/logs");
});

app.get("/api/logs/stream", (req: Request, res: Response) => {
  const token =
    req.query.token ||
    req.headers["authorization"]?.toString().replace(/^Bearer\s+/i, "");
  const coreUrl = new URL("/api/logs/stream", CORE_URL);
  if (token) {
    coreUrl.searchParams.set("token", String(token));
  } else if (ADMIN_TOKEN) {
    coreUrl.searchParams.set("token", ADMIN_TOKEN);
  }

  const sseReq = http.request(
    coreUrl.toString(),
    {
      method: "GET",
      headers: {
        Accept: "text/event-stream",
        ...(token
          ? { Authorization: `Bearer ${token}` }
          : ADMIN_TOKEN
            ? { Authorization: `Bearer ${ADMIN_TOKEN}` }
            : {}),
      },
    },
    (coreRes) => {

      if (coreRes.statusCode !== 200) {
        res.writeHead(coreRes.statusCode || 502, {
          "Content-Type": coreRes.headers["content-type"] || "application/json",
        });
        coreRes.pipe(res);
        return;
      }

      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache, no-transform",
        Connection: "keep-alive",
        "X-Accel-Buffering": "no",
      });

      res.flushHeaders();

      coreRes.on("data", (chunk) => {
        res.write(chunk);
      });

      coreRes.on("end", () => {
        res.end();
      });
    },
  );

  sseReq.on("error", (err) => {
    console.error("[Ferrix Backend] Erro no stream SSE do Core:", err.message);
    if (!res.headersSent) {
      res.status(502).json({ error: "SSE Stream Error", message: err.message });
    }
  });

  req.on("close", () => {
    sseReq.destroy();
  });

  sseReq.end();
});

app.get("/api/config", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/config");
});

app.post("/api/config", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/config", "POST");
});

app.post("/api/config/validate", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/config/validate", "POST");
});

app.post("/api/route-test", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/route-test", "POST");
});

app.post("/api/import/nginx", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/import/nginx", "POST");
});

app.get("/api/vhosts", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/vhosts");
});

app.post("/api/vhosts", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/vhosts", "POST");
});

app.delete("/api/vhosts/:id", (req: Request, res: Response) => {
  proxyToCore(req, res, `/api/vhosts/${req.params.id}`, "DELETE");
});

app.get("/api/upstreams", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/upstreams");
});

app.post("/api/upstreams", (req: Request, res: Response) => {
  proxyToCore(req, res, "/api/upstreams", "POST");
});

app.delete("/api/upstreams/:name", (req: Request, res: Response) => {
  proxyToCore(req, res, `/api/upstreams/${req.params.name}`, "DELETE");
});

const frontendDistPath = path.resolve(__dirname, "../../frontend/dist");

if (fs.existsSync(frontendDistPath)) {
  console.log(
    `[Ferrix Backend] Serving embedded React Dashboard from: ${frontendDistPath}`,
  );
  app.use(express.static(frontendDistPath));

  app.get("*", (req: Request, res: Response, next: NextFunction) => {
    if (req.path.startsWith("/api")) return next();
    res.sendFile(path.join(frontendDistPath, "index.html"));
  });
} else {
  app.get("/", (_req: Request, res: Response) => {
    res.json({
      service: "Ferrix Node.js Management API",
      status: "online",
      core_url: CORE_URL,
      frontend_built: false,
      message:
        "Build the frontend in frontend/ to serve the web interface here.",
    });
  });
}

app.listen(PORT, () => {
  console.log(
    `================================================================`,
  );
  console.log(
    `🚀 Ferrix Backend (Node.js) listening on http://0.0.0.0:${PORT}`,
  );
  console.log(`🎯 Targeting Ferrix Core at: ${CORE_URL}`);
  console.log(
    `================================================================`,
  );
});

