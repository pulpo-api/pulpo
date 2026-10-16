import type { HttpRequest } from "./core";
import type { Protocol, TreeNode, Workspace } from "./workspace";

const json = [{ name: "Content-Type", value: "application/json", enabled: true }];

function request(
  id: string,
  name: string,
  req: Partial<HttpRequest> & Pick<HttpRequest, "method" | "url">,
  protocol: Protocol = "http",
): TreeNode {
  return { type: "request", id, name, protocol, request: { headers: [], body: null, ...req } };
}

/** The browser preview has no file system, so it works on this in-memory copy of examples/jsonplaceholder. */
export function sampleWorkspace(): Workspace {
  return {
    root: "examples/jsonplaceholder",
    name: "JSONPlaceholder (browser preview)",
    tree: [
      {
        type: "folder",
        id: "01-posts",
        name: "Posts",
        children: [
          request("01-posts/01-list-posts.pulpo.yaml", "List posts", {
            method: "GET",
            url: "https://jsonplaceholder.typicode.com/posts?_limit=5",
          }),
          request("01-posts/02-get-a-post.pulpo.yaml", "Get a post", {
            method: "GET",
            url: "https://jsonplaceholder.typicode.com/posts/1",
          }),
          request("01-posts/03-create-a-post.pulpo.yaml", "Create a post", {
            method: "POST",
            url: "https://jsonplaceholder.typicode.com/posts",
            headers: json,
            body: '{\n  "title": "Hello from Pulpo",\n  "body": "Eight arms, one client.",\n  "userId": 1\n}\n',
          }),
          request("01-posts/04-delete-a-post.pulpo.yaml", "Delete a post", {
            method: "DELETE",
            url: "https://jsonplaceholder.typicode.com/posts/1",
          }),
        ],
      },
      {
        type: "folder",
        id: "02-users",
        name: "Users",
        children: [
          request("02-users/01-list-users.pulpo.yaml", "List users", {
            method: "GET",
            url: "https://jsonplaceholder.typicode.com/users",
          }),
        ],
      },
    ],
  };
}
