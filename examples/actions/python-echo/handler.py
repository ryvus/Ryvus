from ryvus import api_action


@api_action
def handler(event, context):
    print("Received context:", context)

    return {
        "message": "Hello, Ryvus python SDK!",
    }