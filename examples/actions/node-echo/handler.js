let raw = "";

process.stdin.setEncoding("utf8");

process.stdin.on("data", (chunk) => {
  raw += chunk;
});

process.stdin.on("end", () => {
  const request = JSON.parse(raw);

  const result = {
    protocol_version: request.protocol_version,
    invocation_id: request.invocation_id,
    status: "success",
    output: {
      received: request.event,
      handled_by: "node",
    },
    error: null,
  };

  process.stdout.write(JSON.stringify(result));
});