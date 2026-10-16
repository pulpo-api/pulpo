pm.test("Responds in under 3 s", () => pm.expect(pm.response.responseTime).to.be.below(3000));
