pm.test("Returns 5 posts", () => pm.expect(pm.response.json()).to.have.lengthOf(5));
