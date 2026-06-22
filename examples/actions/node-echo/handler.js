// @ryvus-sdk/api
import { apiAction } from "../../../sdk/node/dist/index.js";

export default apiAction((event, context) => {
  console.log("hello from node");
  console.warn("this is a warning");
  console.info("this is some info");
  console.error("this is an error");
  console.trace("this is a trace");

  return {
    message: "Hello from Ryvus Node SDK",
  };
});