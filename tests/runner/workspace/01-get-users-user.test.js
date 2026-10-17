pm.test("asks for the user of the iteration", () => {
  pm.expect(pm.response.json().path).to.equal("/users/" + pm.iterationData.get("user"));
});
// The echo server answers with the request; headers change with the Pulpo version.
pm.response.to.matchSnapshot("user-" + pm.iterationData.get("user"), { ignore: ["$.headers"] });
