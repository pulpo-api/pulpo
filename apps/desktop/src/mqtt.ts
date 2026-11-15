// MQTT 3.1.1 over WebSocket for the browser preview (pulpo-core's mqtt.rs does TCP, TLS
// and WebSocket in the app). Only what testing needs: QoS 0/1, retain, user/password.

const encoder = new TextEncoder();

function str(s: string): number[] {
  const bytes = [...encoder.encode(s)];
  return [bytes.length >> 8, bytes.length & 0xff, ...bytes];
}

function packet(first: number, body: number[]): Uint8Array {
  const out = [first];
  let len = body.length;
  do {
    let byte = len % 128;
    len = Math.floor(len / 128);
    if (len > 0) byte |= 0x80;
    out.push(byte);
  } while (len > 0);
  return new Uint8Array([...out, ...body]);
}

export function connectPacket(clientId: string, username: string, password: string, keepAlive = 30): Uint8Array {
  let flags = 0x02;
  if (username) flags |= 0x80;
  if (username && password) flags |= 0x40;
  const body = [...str("MQTT"), 4, flags, keepAlive >> 8, keepAlive & 0xff, ...str(clientId)];
  if (username) body.push(...str(username));
  if (username && password) body.push(...str(password));
  return packet(0x10, body);
}

export function subscribePacket(id: number, topics: string[], qos: number): Uint8Array {
  const body = [id >> 8, id & 0xff];
  for (const t of topics) body.push(...str(t), Math.min(qos, 1));
  return packet(0x82, body);
}

export function publishPacket(id: number, topic: string, payload: string, qos: number, retain: boolean): Uint8Array {
  const q = Math.min(qos, 1);
  const body = [...str(topic)];
  if (q > 0) body.push(id >> 8, id & 0xff);
  body.push(...encoder.encode(payload));
  return packet(0x30 | (q << 1) | (retain ? 1 : 0), body);
}

export type Incoming =
  | { kind: "connack"; code: number }
  | { kind: "suback"; codes: number[] }
  | { kind: "publish"; topic: string; payload: string; qos: number; retain: boolean; id: number | null }
  | { kind: "other"; type: number };

/** Splits a byte stream into packets; keeps what is incomplete for the next feed. */
export class MqttReader {
  private buffer = new Uint8Array(0);

  feed(bytes: Uint8Array): Incoming[] {
    const joined = new Uint8Array(this.buffer.length + bytes.length);
    joined.set(this.buffer);
    joined.set(bytes, this.buffer.length);
    this.buffer = joined;
    const out: Incoming[] = [];
    for (;;) {
      if (this.buffer.length < 2) break;
      let len = 0;
      let mult = 1;
      let i = 1;
      let complete = false;
      while (i < this.buffer.length && i <= 4) {
        const b = this.buffer[i++];
        len += (b & 0x7f) * mult;
        mult *= 128;
        if (!(b & 0x80)) {
          complete = true;
          break;
        }
      }
      if (!complete || this.buffer.length < i + len) break;
      const first = this.buffer[0];
      const body = this.buffer.slice(i, i + len);
      this.buffer = this.buffer.slice(i + len);
      out.push(decode(first, body));
    }
    return out;
  }
}

function decode(first: number, body: Uint8Array): Incoming {
  const type = first >> 4;
  if (type === 2) return { kind: "connack", code: body[1] ?? 255 };
  if (type === 9) return { kind: "suback", codes: [...body.slice(2)] };
  if (type === 3) {
    const qos = (first >> 1) & 3;
    const tlen = (body[0] << 8) | body[1];
    const topic = new TextDecoder().decode(body.slice(2, 2 + tlen));
    let at = 2 + tlen;
    let id: number | null = null;
    if (qos > 0) {
      id = (body[at] << 8) | body[at + 1];
      at += 2;
    }
    return { kind: "publish", topic, payload: new TextDecoder().decode(body.slice(at)), qos, retain: (first & 1) === 1, id };
  }
  return { kind: "other", type };
}

export function pubackPacket(id: number): Uint8Array {
  return packet(0x40, [id >> 8, id & 0xff]);
}

export const PINGREQ = new Uint8Array([0xc0, 0]);
export const DISCONNECT = new Uint8Array([0xe0, 0]);

export const CONNACK_REASONS: Record<number, string> = {
  1: "unacceptable protocol version",
  2: "client id rejected",
  3: "server unavailable",
  4: "bad user name or password",
  5: "not authorized",
};
