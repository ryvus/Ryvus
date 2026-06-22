import logging as logger
from ryvus import api_action


@api_action
def handler(event, context):
    print("normal print statement")
    logger.debug("debug message")
    logger.info("info message")
    logger.warning("warn message")
    logger.error("error message")

    return {
        "message": "Hello, Ryvus python SDK!",
    }