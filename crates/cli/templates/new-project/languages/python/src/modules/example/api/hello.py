from ryvus import api_action

@api_action(
    method="GET",
    path="/hello"
)
def hello(event):
    return {
        "message": "Hello from Ryvus Python!"
    }