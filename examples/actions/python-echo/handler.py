import logging
from ryvus import api_action

logger = logging.getLogger(__name__)

@api_action
def handler(event, context):
    print("hello from print")

    logger.debug("debug from python")
    logger.info("info from python")
    logger.warning("warning from python")
    logger.error("error from python")

    return {
        "message": "Hello from Ryvus Python SDK"
    }