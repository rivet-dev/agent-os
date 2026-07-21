import json
import subprocess

completed = subprocess.run(
    ["agentos-weather", "forecast", "--city", "Paris", "--days", "3"],
    check=True,
    capture_output=True,
    text=True,
)

response = json.loads(completed.stdout)
print(response["result"])
