import type { HttpRequest } from "./core";
import type { Environment, Protocol, TreeNode, Workspace } from "./workspace";

const json = [{ name: "Content-Type", value: "application/json", enabled: true }];

function request(
  id: string,
  name: string,
  req: Partial<HttpRequest> & Pick<HttpRequest, "method" | "url">,
  protocol: Protocol = "http",
  test = "",
): TreeNode {
  return { type: "request", id, name, protocol, request: { headers: [], body: null, disabledParams: [], pathParams: [], ...req }, scripts: { pre: "", test } };
}

/** The browser preview has no file system, so it works on this in-memory copy of examples/jsonplaceholder. */
export function sampleWorkspace(): Workspace {
  return {
    root: "examples/jsonplaceholder",
    name: "JSONPlaceholder (browser preview)",
    // Sent by every request: a common header of the collection.
    headers: [{ name: "Accept", value: "application/json", enabled: true }],
    tree: [
      {
        type: "folder",
        id: "01-posts",
        name: "Posts",
        children: [
          request("01-posts/01-list-posts.pulpo.yaml", "List posts", {
            method: "GET",
            url: "{{base}}/posts?_limit=5",
            disabledParams: [{ name: "userId", value: "{{userId}}" }],
          }),
          request("01-posts/02-get-a-post.pulpo.yaml", "Get a post", {
            method: "GET",
            url: "{{base}}/posts/:postId",
            pathParams: [{ name: "postId", value: "1" }],
          }),
          request("01-posts/03-create-a-post.pulpo.yaml", "Create a post", {
            method: "POST",
            url: "{{base}}/posts",
            headers: json,
            body: '{\n  "title": "Hello from Pulpo",\n  "body": "Eight arms, one client.",\n  "userId": {{userId}}\n}\n',
          }, "http", 'pm.test("Created", () => pm.response.to.have.status(201));\nconst post = pm.response.json();\npm.environment.set("lastPostId", post.id);\n'),
          request("01-posts/04-delete-a-post.pulpo.yaml", "Delete a post", {
            method: "DELETE",
            url: "{{base}}/posts/1",
          }),
        ],
      },
      {
        type: "folder",
        id: "02-users",
        name: "Users",
        // Shows auth inheritance; JSONPlaceholder ignores the header, so a demo value is enough.
        auth: { type: "bearer", token: "{{token}}" },
        settings: { timeoutMs: 10000 },
        children: [
          request("02-users/01-list-users.pulpo.yaml", "List users", {
            method: "GET",
            url: "{{base}}/users",
          }),
        ],
      },
    ],
  };
}

export function sampleEnvironments(): Environment[] {
  return [
    {
      id: "environments/public.yaml",
      name: "Public",
      variables: [
        { name: "base", value: "https://jsonplaceholder.typicode.com", secret: false, hasValue: true },
        { name: "userId", value: "1", secret: false, hasValue: true },
        { name: "token", value: "demo-token", secret: false, hasValue: true },
      ],
    },
  ];
}
