pm.test("Created", () => pm.response.to.have.status(201));
const post = pm.response.json();
pm.test("Echoes the title", () => pm.expect(post.title).to.equal("Hello from Pulpo"));
pm.environment.set("lastPostId", post.id);
