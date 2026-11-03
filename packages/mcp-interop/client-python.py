#!/usr/bin/env python
"""A5-03 真实 MCP 客户端（Python）。

用官方 `mcp` PyPI 包（与 Node 客户端是完全独立的实现）走 stdio 连到
packages/mcp-bridge。两者共用同一套 CLI 约定，方便 Rust 集成测试用同一份
剧本驱动，从而证明「客户端实现不同只影响体验，不改变业务授权」。

用法：
  python client-python.py --mode discover --endpoint <path> --connection <id> --bridge <path>
  python client-python.py --mode status   ... --operation-id <id>
"""

import argparse
import asyncio
import json
import sys

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


def unwrap(result):
    """桥接把工具结果包成 content[0].text；错误则是 "CODE: message"。"""
    text = None
    for item in getattr(result, "content", None) or []:
        if getattr(item, "type", None) == "text":
            text = item.text
            break
    if text is None:
        raise RuntimeError(f"工具返回缺少文本内容：{result!r}")
    if getattr(result, "isError", False):
        code, _, message = text.partition(":")
        error = RuntimeError(message.strip() or text)
        error.mcp_code = code.strip()
        raise error
    return json.loads(text)


async def call_tool(session, name, arguments):
    return unwrap(await session.call_tool(name, arguments))


async def run(options):
    parameters = StdioServerParameters(
        command=options.node,
        args=[options.bridge],
        env={
            "TEACELL_MCP_ENDPOINT": options.endpoint,
            "TEACELL_MCP_CONNECTION": options.connection,
        },
    )
    report = {"client": "python:mcp"}
    async with stdio_client(parameters) as (read_stream, write_stream):
        async with ClientSession(read_stream, write_stream) as session:
            initialized = await session.initialize()
            report["server"] = {
                "name": initialized.serverInfo.name,
                "version": initialized.serverInfo.version,
            }
            try:
                if options.mode == "discover":
                    listed = await session.list_tools()
                    report["tools"] = sorted(tool.name for tool in listed.tools)
                    report["toolCount"] = len(listed.tools)
                elif options.mode == "prepare":
                    search = await call_tool(session, "tea_assets_search", {"text": ""})
                    report["selectionRef"] = search["selectionRef"]
                    report["total"] = search["total"]
                    prepared = await call_tool(
                        session,
                        "tea_assets_trash_prepare",
                        {"selectionRef": search["selectionRef"]},
                    )
                    report["planId"] = prepared["planId"]
                    report["reviewRef"] = prepared["reviewRef"]
                    report["itemCount"] = prepared["itemCount"]
                    report["originKind"] = prepared["originKind"]
                    report["originLabel"] = prepared.get("originLabel")
                elif options.mode == "status":
                    # 全新进程、全新连接、没有任何本地状态：只凭稳定的 operationId 查询。
                    if not options.operation_id:
                        raise RuntimeError("status 模式需要 --operation-id")
                    snapshot = await call_tool(
                        session, "tea_operations_get", {"operationId": options.operation_id}
                    )
                    report["operationId"] = snapshot["operationId"]
                    report["state"] = snapshot["state"]
                    report["phase"] = snapshot["phase"]
                    report["completed"] = snapshot["completed"]
                    report["failed"] = snapshot["failed"]
                    report["total"] = snapshot.get("total")
                    report["canCancel"] = snapshot["canCancel"]
                    report["canResume"] = snapshot["canResume"]
                elif options.mode in ("submit", "submit-unapproved"):
                    if not options.plan_id:
                        raise RuntimeError("submit 模式需要 --plan-id")
                    # 幂等键是 ResourceRef：契约要求 16..=128 字节，短于 16 会被宿主
                    # 按 INVALID_INPUT 拒绝（不是权限问题，也不是宿主不可用）。
                    key = options.idempotency_key or f"interop_default_{options.mode}"
                    submitted = await call_tool(
                        session,
                        "tea_assets_trash_submit",
                        {"planId": options.plan_id, "idempotencyKey": key},
                    )
                    operation = submitted["operation"]
                    report["operationId"] = operation["operationId"]
                    report["replayed"] = submitted["replayed"]
                    snapshot = await call_tool(
                        session, "tea_operations_get", {"operationId": operation["operationId"]}
                    )
                    report["state"] = snapshot["state"]
                    report["total"] = snapshot.get("total")
                    report["completed"] = snapshot["completed"]
                else:
                    raise RuntimeError(f"未知模式：{options.mode}")
                report["ok"] = True
            except Exception as error:  # noqa: BLE001 - 客户端要把宿主错误如实回传
                report["ok"] = False
                report["code"] = getattr(error, "mcp_code", None) or "UNKNOWN"
                report["message"] = str(error)
    return report


class ArgumentError(RuntimeError):
    """参数错误也必须走统一的报告格式，不能只往 stderr 吐 usage 然后静默退出。"""


class ReportingParser(argparse.ArgumentParser):
    """argparse 默认 `sys.exit(2)` 且不写 stdout。

    Node 客户端对同样的错误会输出一行 `{"ok":false,"code":"CLIENT_FAILURE"}`。
    两个客户端共用同一套 CLI 约定，就必须连失败格式也一致：否则调用方（Rust 集成
    测试）看到的是「退出码非零 + 空输出」，无法区分「剧本跑错」和「客户端崩溃」。
    """

    def error(self, message):
        raise ArgumentError(message)


def main():
    # Windows 上 stdout 被重定向到管道时，Python 默认用区域编码（cp936），
    # 报告里的中文（originLabel 等）会写成非 UTF-8 字节，调用方按 UTF-8 读会
    # 直接判成空输出。单个 JSON 报告的编码必须是确定的。
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):  # pragma: no cover - 老解释器/被替换的流
        pass
    parser = ReportingParser()
    parser.add_argument("--mode", required=True)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--connection", required=True)
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--node", required=True)
    parser.add_argument("--plan-id", dest="plan_id")
    parser.add_argument("--idempotency-key", dest="idempotency_key")
    parser.add_argument("--operation-id", dest="operation_id")
    try:
        options = parser.parse_args()
        report = asyncio.run(run(options))
    except ArgumentError as error:
        report = {"ok": False, "code": "CLIENT_FAILURE", "message": str(error)}
    except Exception as error:  # noqa: BLE001
        report = {"ok": False, "code": "CLIENT_FAILURE", "message": str(error)}
    sys.stdout.write(json.dumps(report, ensure_ascii=False) + "\n")
    sys.stdout.flush()
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
