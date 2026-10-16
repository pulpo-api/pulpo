// Fetch the author once and keep it as a local value for every request in this folder.
if (!pm.variables.get("authorName")) {
  pm.sendRequest("{{base}}/users/{{userId}}", (err, res) => {
    if (err) throw err;
    pm.environment.set("authorName", res.json().name);
  });
}
