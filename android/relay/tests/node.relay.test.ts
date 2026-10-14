import { afterAll, describe, expect, it, vi } from "vitest";
import { WebSocket } from "ws";
import { startRelay } from "../tools/dev-relay.ts";
import { ACCESS, behaviour, offer, open, proofOf, randomRoom, type Conn, type Harness, type Msg, type Options } from "./behaviour.ts";

let relay: Awaited<ReturnType<typeof startRelay>>;

function wrap(ws: WebSocket): Conn {
  const queue: Msg[] = [];
  let wake: (() => void) | undefined;
  const push = (m: Msg) => {
    queue.push(m);
    wake?.();
  };
  ws.on("message", (data, isBinary) => {
    const buf = Buffer.isBuffer(data) ? data : Buffer.concat(data as Buffer[]);
    push(isBinary ? { kind: "binary", data: new Uint8Array(buf) } : { kind: "text", data: buf.toString() });
  });
  ws.on("close", (code, reason) => push({ kind: "close", code, reason: reason.toString() }));
  const next = async (timeoutMs = 1500): Promise<Msg> => {
    const until = Date.now() + timeoutMs;
    while (queue.length === 0) {
      if (Date.now() > until) throw new Error("timed out waiting for a message");
      await new Promise<void>((r) => {
        wake = r;
        setTimeout(r, 25);
      });
    }
    return queue.shift()!;
  };
  return {
    next,
    quiet: async (ms = 200) => {
      const until = Date.now() + ms;
      while (Date.now() < until) {
        if (queue.length > 0) return false;
        await new Promise((r) => setTimeout(r, 15));
      }
      return queue.length === 0;
    },
    send: (d) => ws.send(d),
    close: (code, reason) => ws.close(code ?? 1000, reason ?? ""),
  };
}

const harness = async (): Promise<Harness> => {
  relay = await startRelay({ accessKey: ACCESS });
  const base = `127.0.0.1:${relay.port}`;
  return {
    async http(path, method = "GET") {
      const r = await fetch(`http://${base}${path}`, { method });
      return { status: r.status, text: await r.text() };
    },
    connect(room, role, o: Options = {}) {
      const protocols = o.protocols ?? offer(o.proof ?? proofOf(1));
      const headers: Record<string, string> = {};
      if (o.key !== null) headers.Authorization = `Bearer ${o.key ?? ACCESS}`;
      return new Promise((resolve) => {
        const ws = new WebSocket(`ws://${base}/v1/room/${room}?role=${role}`, protocols ? protocols.split(",").map((s) => s.trim()).filter(Boolean) : [], { headers });
        ws.on("unexpected-response", (_req, res) => {
          resolve({ refused: res.statusCode ?? 0 });
          ws.terminate();
        });
        ws.on("error", () => {});
        ws.on("open", () => resolve(wrap(ws)));
      });
    },
    setAccessKey: async (key) => relay.setAccessKey(key),
    roomsCreated: async () => relay.rooms(),
    stop: () => relay.close(),
  };
};

behaviour("Node twin", harness);

describe("Node twin only", () => {
  it("keeps bursts in wire order even when asynchronous access checks would finish backwards", async () => {
    const h = await harness();
    try {
      const room = randomRoom();
      const pc = await open(h, room, "pc");
      const phone = await open(h, room, "phone");
      await pc.next();
      await phone.next();
      const original = crypto.subtle.digest.bind(crypto.subtle);
      const pending: (() => Promise<void>)[] = [];
      // Connect first, then hold any per-frame digest completions. Assign slots on CALL so crypto worker timing cannot
      // affect the test; release each real digest in reverse wire order. A synchronous frame path needs no releases.
      const digest = vi.spyOn(crypto.subtle, "digest").mockImplementation((...args) => {
        const result = original(...args);
        return new Promise<ArrayBuffer>((resolve, reject) => {
          pending.push(() => result.then(resolve, reject));
          if (pending.length === 5) {
            void (async () => {
              for (let i = pending.length - 1; i >= 0; i--) await pending[i]();
            })();
          }
        });
      });
      try {
        const frames = Array.from({ length: 5 }, (_, i) => new Uint8Array(40_090).fill(i));
        for (const [sender, receiver] of [[pc, phone], [phone, pc]]) {
          for (const frame of frames) sender.send(frame);
          for (const frame of frames) expect(await receiver.next()).toEqual({ kind: "binary", data: frame });
        }
      } finally {
        digest.mockRestore();
      }
    } finally {
      await h.stop();
    }
  });

  it("matches the shared access generation for every admitted key and revokes removed keys immediately", async () => {
    // Admission uses the shared WebCrypto generation; forwarding must compute the same tag, including after rotation.
    const h = await harness();
    try {
      for (const key of [ACCESS, "a".repeat(16), "_-09AZaz".repeat(16)]) {
        await h.setAccessKey(key);
        const room = randomRoom();
        const pc = await open(h, room, "pc", { key });
        const phone = await open(h, room, "phone", { key });
        await pc.next();
        await phone.next();
        pc.send(new Uint8Array([42]));
        expect(await phone.next()).toEqual({ kind: "binary", data: new Uint8Array([42]) });
        await h.setAccessKey(undefined);
        pc.send(new Uint8Array([43]));
        expect(await pc.next()).toEqual({ kind: "close", code: 1008, reason: "access key changed" });
        expect(await phone.next()).toEqual({ kind: "text", data: '{"peer":"offline"}' });
        phone.close();
      }
    } finally {
      await h.stop();
    }
  });

  it("answers 429 after too many new connections from one address, before checking the key", async () => {
    const r = await startRelay({ accessKey: ACCESS, connectsPerMinute: 3 });
    try {
      const statuses: number[] = [];
      for (let i = 0; i < 5; i++) {
        statuses.push(
          await new Promise<number>((resolve) => {
            const ws = new WebSocket(`ws://127.0.0.1:${r.port}/v1/room/${randomRoom()}?role=pc`, ["coucou.v1", `coucou.join.${proofOf(1)}`], { headers: { Authorization: "Bearer wrong-wrong-wrong-wrong" } });
            ws.on("unexpected-response", (_q, res) => (resolve(res.statusCode ?? 0), ws.terminate()));
            ws.on("error", () => {});
          }),
        );
      }
      expect(statuses).toEqual([401, 401, 401, 429, 429]);
    } finally {
      await r.close();
    }
  });

  it("leaves no room behind when everyone has left", async () => {
    const r = await startRelay({ accessKey: ACCESS });
    try {
      const ws = new WebSocket(`ws://127.0.0.1:${r.port}/v1/room/${randomRoom()}?role=pc`, ["coucou.v1", `coucou.join.${proofOf(1)}`], { headers: { Authorization: `Bearer ${ACCESS}` } });
      await new Promise((res) => ws.on("open", res));
      expect(r.rooms()).toBe(1);
      ws.close();
      await new Promise((res) => ws.on("close", res));
      await new Promise((res) => setTimeout(res, 50));
      expect(r.rooms()).toBe(0);
    } finally {
      await r.close();
    }
  });
});
afterAll(() => undefined);
