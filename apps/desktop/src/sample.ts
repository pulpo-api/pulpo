import type { HttpRequest } from "./core";

export type Protocol = "http" | "graphql" | "grpc" | "websocket" | "soap" | "sse";

export interface RequestItem {
  id: string;
  name: string;
  protocol: Protocol;
  request: HttpRequest;
}

export interface Folder {
  name: string;
  items: RequestItem[];
}

const json = [{ name: "Content-Type", value: "application/json", enabled: true }];

function http(id: string, name: string, request: Partial<HttpRequest> & Pick<HttpRequest, "method" | "url">): RequestItem {
  return { id, name, protocol: "http", request: { headers: [], body: null, ...request } };
}

function planned(id: string, name: string, protocol: Protocol, url: string): RequestItem {
  return { id, name, protocol, request: { method: "GET", url, headers: [], body: null } };
}

export const sampleCollection: Folder[] = [
  {
    name: "01 Posts",
    items: [
      http("list-posts", "List posts", { method: "GET", url: "https://jsonplaceholder.typicode.com/posts?_limit=5" }),
      http("get-post", "Get a post", { method: "GET", url: "https://jsonplaceholder.typicode.com/posts/1" }),
      http("create-post", "Create a post", {
        method: "POST",
        url: "https://jsonplaceholder.typicode.com/posts",
        headers: json,
        body: '{\n  "title": "Hello from Pulpo",\n  "body": "Eight arms, one client.",\n  "userId": 1\n}',
      }),
      http("delete-post", "Delete a post", { method: "DELETE", url: "https://jsonplaceholder.typicode.com/posts/1" }),
    ],
  },
  {
    name: "02 Echo",
    items: [
      http("echo-headers", "Echo my headers", {
        method: "GET",
        url: "https://httpbin.org/headers",
        headers: [{ name: "X-Tenant-ID", value: "test", enabled: true }],
      }),
      http("not-found", "A 404", { method: "GET", url: "https://httpbin.org/status/404" }),
    ],
  },
  {
    name: "03 Coming soon",
    items: [
      planned("order-graphql", "GraphQL query", "graphql", "https://example.com/graphql"),
      planned("order-grpc", "gRPC call", "grpc", "orders.v1.OrderService/Track"),
      planned("courier-ws", "WebSocket", "websocket", "ws://localhost:3005/ws"),
      planned("soap-trademark", "SOAP operation", "soap", "https://example.com/Service?wsdl"),
    ],
  },
];
