// @ryvus-sdk/api
import { apiAction } from "../../../sdk/node/dist/index.js";

export default apiAction((event, context) => {
  console.log("hello from node");

  return {
    message: "Hello from Ryvus Node SDK",
  };
});