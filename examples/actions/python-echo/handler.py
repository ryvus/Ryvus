from ryvus import api_action


@api_action
def handler(event):
    return {
        "message": "Hello, Ryvus python SDK!"
    }