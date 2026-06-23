import { apiAction } from "@ryvus/sdk";

export default apiAction(
  async (event, context) => {
    return {
      message: "Hello from Ryvus Node!",
    };
  },
  {
    method: "GET",
    path: "/hello",
  },
);
