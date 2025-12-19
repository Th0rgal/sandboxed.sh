#!/usr/bin/env python3
"""Check all security analysis task results."""

import json
import requests
import os

API_URL = "https://agent-backend.thomas.md"

# All security analysis task IDs (latest run after service restart)
TASKS = {
    "moonshotai/kimi-k2-thinking": "ec103234-9fe5-4814-ab24-58dd7856dd43",
    "x-ai/grok-4.1-fast": "97e654e1-a381-49da-911b-1b835449bb55",
    "google/gemini-3-flash-preview": "989ecdb8-b900-44d8-a9e5-a9e9394a9077",
    "deepseek/deepseek-v3.2": "7a589315-9b9b-4805-9e14-da01224717e1",
    "qwen/qwen3-vl-235b-a22b-thinking": "d19711d5-3158-48cd-aa88-81bb4d22262c",
    "mistralai/mistral-large-2512": "e0c3b62e-7b64-425f-8289-0a1b274e5dd4",
    "amazon/nova-pro-v1": "3fe39368-a7fe-4852-961f-44863128b426",
    "z-ai/glm-4.6v": "eb161e08-c923-44b5-8c8d-6f0d01366082",
    "anthropic/claude-sonnet-4.5": "8943e1ef-2c95-4485-bcee-8d3bc611fa6d",
}


def get_token():
    """Get auth token."""
    secrets_path = os.path.join(os.path.dirname(__file__), "..", "secrets.json")
    password = ""
    if os.path.exists(secrets_path):
        with open(secrets_path) as f:
            secrets = json.load(f)
            password = secrets.get("auth", {}).get("dashboard_password", "")
    if not password:
        password = os.environ.get("DASHBOARD_PASSWORD", "")
    
    if not password:
        print("Error: No dashboard password found")
        return None
    
    resp = requests.post(f"{API_URL}/api/auth/login", json={"password": password})
    return resp.json().get("token")


def check_task(token, model, task_id):
    """Check a task's status."""
    headers = {"Authorization": f"Bearer {token}"}
    try:
        resp = requests.get(f"{API_URL}/api/task/{task_id}", headers=headers)
        data = resp.json()
        return {
            "model": model,
            "task_id": task_id,
            "status": data.get("status", "unknown"),
            "iterations": data.get("iterations", 0),
            "result_length": len(data.get("result") or ""),
            "result_preview": (data.get("result") or "")[:200],
            "error": "Error:" in (data.get("result") or ""),
        }
    except Exception as e:
        return {
            "model": model,
            "task_id": task_id,
            "status": "error",
            "iterations": 0,
            "result_length": 0,
            "result_preview": str(e),
            "error": True,
        }


def main():
    token = get_token()
    if not token:
        return
    
    print("=" * 80)
    print("Security Analysis Task Status")
    print("=" * 80)
    print()
    
    results = []
    for model, task_id in TASKS.items():
        result = check_task(token, model, task_id)
        results.append(result)
    
    # Print summary table
    print(f"{'Model':<40} | {'Status':<10} | {'Iters':<5} | {'Chars':<8}")
    print("-" * 40 + "-+-" + "-" * 10 + "-+-" + "-" * 5 + "-+-" + "-" * 8)
    
    for r in results:
        print(f"{r['model']:<40} | {r['status']:<10} | {r['iterations']:<5} | {r['result_length']:<8}")
    
    # Categorize
    completed = [r for r in results if r["status"] == "completed" and not r["error"]]
    failed = [r for r in results if r["status"] == "failed" or r["error"]]
    running = [r for r in results if r["status"] in ("pending", "running")]
    
    print()
    print("=" * 80)
    print(f"Summary: {len(completed)} completed, {len(running)} running, {len(failed)} failed")
    print("=" * 80)
    
    if completed:
        print(f"\n✓ Completed ({len(completed)}):")
        for r in completed:
            preview = r['result_preview'].replace('\n', ' ')[:100]
            print(f"  - {r['model']}: {preview}...")
    
    if running:
        print(f"\n⏳ Running ({len(running)}):")
        for r in running:
            print(f"  - {r['model']}")
    
    if failed:
        print(f"\n❌ Failed ({len(failed)}):")
        for r in failed:
            preview = r['result_preview'].replace('\n', ' ')[:100]
            print(f"  - {r['model']}: {preview}...")


if __name__ == "__main__":
    main()
