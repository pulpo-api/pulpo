export type Protocol = "http" | "graphql" | "grpc" | "websocket" | "soap" | "sse";

export interface RequestItem {
  id: string;
  name: string;
  protocol: Protocol;
  method?: string;
  url: string;
}

export interface Folder {
  name: string;
  items: RequestItem[];
}

export const sampleCollection: Folder[] = [
  {
    name: "01 Authentication",
    items: [
      { id: "login", name: "Log in", protocol: "http", method: "POST", url: "{{gateway}}/api/v1/security/login" },
      { id: "me", name: "Who am I", protocol: "http", method: "GET", url: "{{gateway}}/api/v1/me" },
    ],
  },
  {
    name: "02 Orders",
    items: [
      { id: "create-order", name: "Create order", protocol: "http", method: "POST", url: "{{orders}}/api/v1/checkouts" },
      { id: "order-graphql", name: "Order details", protocol: "graphql", url: "{{orders}}/graphql" },
      { id: "order-grpc", name: "Track order", protocol: "grpc", url: "orders.v1.OrderService/Track" },
    ],
  },
  {
    name: "03 Realtime",
    items: [
      { id: "courier-ws", name: "Courier position", protocol: "websocket", url: "ws://localhost:3005/ws" },
      { id: "events-sse", name: "Order events", protocol: "sse", url: "{{orders}}/events/stream" },
    ],
  },
  {
    name: "04 Legacy",
    items: [{ id: "soap-trademark", name: "Search trademark", protocol: "soap", url: "{{legacy}}/TrademarkService?wsdl" }],
  },
];
