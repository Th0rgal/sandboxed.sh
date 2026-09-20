"""Exercise the patched Hermes REST handlers with real temporary cron storage.

PYTHONPATH must point at Hermes 882881de with orb-cron-api-fields.patch applied.
Requires aiohttp plus the dependencies used by generate.py. Only the handler
methods are loaded, avoiding gateway initialization, credentials and sessions.
Authentication/availability are bypassed by this local test harness; this tests
field transport and storage, not authentication or scheduler execution.
"""
import ast
import asyncio
import os
import tempfile
from pathlib import Path
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

async def verify():
    from cron import jobs
    source = Path(jobs.__file__).parents[1] / "gateway/platforms/api_server.py"
    tree = ast.parse(source.read_text())
    adapter = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "APIServerAdapter")
    methods = {"_handle_create_job", "_handle_update_job", "_job_response", "_validate_cron_prompt", "_cron_error_response"}
    constants = {"_UPDATE_ALLOWED_FIELDS", "_MAX_NAME_LENGTH", "_MAX_PROMPT_LENGTH"}
    adapter.bases = []
    adapter.body = [n for n in adapter.body if (isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and n.name in methods) or (isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id in constants for t in n.targets))]
    namespace = {"web": web, "Optional": __import__('typing').Optional,
                 "_cron_create": jobs.create_job, "_cron_update": jobs.update_job,
                 "_scan_cron_prompt": None, "_redact_api_error_text": str,
                 "_notify_cron_provider_jobs_changed": lambda: None,
                 "_CronSchedulerRegistrationError": type("UnusedRegistrationError", (Exception,), {})}
    exec(compile(ast.Module(body=[adapter], type_ignores=[]), str(source), "exec"), namespace)
    adapter_type = namespace["APIServerAdapter"]
    adapter_type._cron_request_guard = lambda self, request, **kwargs: (request.match_info.get("job_id"), None)
    adapter_type._cron_origin_from_request = lambda self, request: {"platform": "api_server", "chat_id": "api"}
    instance = adapter_type()
    app = web.Application()
    app.router.add_post("/api/jobs", instance._handle_create_job)
    app.router.add_patch("/api/jobs/{job_id}", instance._handle_update_job)
    async with TestClient(TestServer(app)) as client:
        payload = dict(name="REST fixture", prompt="Summarize local notes", schedule="every 1h",
                       repeat=8, skills=["project-notes"], model="gpt-5", provider="openai",
                       reasoning_effort="high", workdir=os.environ["HERMES_HOME"],
                       context_from=["self"], failure_deliver="local", deliver="local")
        response = await client.post("/api/jobs", json=payload)
        body = await response.json()
        assert response.status == 200, body
        job = body["job"]
        for key in ("model", "provider", "reasoning_effort", "workdir", "context_from", "failure_deliver", "skills"):
            assert job[key] == payload[key], (key, job[key])
        assert job["repeat"] == {"times": 8, "completed": 0}
        response = await client.patch(f'/api/jobs/{job["id"]}', json={"repeat": 0, "reasoning_effort": "low", "context_from": [], "workdir": ""})
        body = await response.json()
        assert response.status == 200, body
        result = body["job"]
        assert result["repeat"] == {"times": None, "completed": 0}
        assert result["reasoning_effort"] == "low"
        assert not result["context_from"] and result["workdir"] is None
        print("PASS: patched real Hermes create/update handlers preserve advanced fields and numeric repeat")

with tempfile.TemporaryDirectory(prefix="orb-hermes-api-test-") as home:
    os.environ["HERMES_HOME"] = home
    asyncio.run(verify())
