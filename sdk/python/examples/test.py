from ryvus import action

@action("hello", description="Say hello")
def hello(input):
    return {"message": f"Hello {input['name']}"}

print(hello({"name": "Maikel"}))
print(hello.__ryvus_action__)