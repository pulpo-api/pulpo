pm.test("the body is sent as JSON", () => {
  pm.expect(JSON.parse(pm.response.json().body)).to.have.jsonSchema({
    type: "object",
    required: ["user", "qty"],
    properties: { user: { type: "string" }, qty: { type: "integer", minimum: 1 } },
  });
});
