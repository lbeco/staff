# Staff TUI Regression Eval

这些用例来自真实 TUI 使用过程中暴露的问题。运行方式：

```bash
staff eval run --suite tui_regression
```

每个 case 都会真实调用 DeepSeek，不使用 mock。`expect` 字段用于做轻量自动评分。

## Case: answer-model-identity

expect: answer_only, mentions_deepseek, no_file_change

### User

你是什么模型

## Case: chinese-calculator-design

expect: answer_only, chinese, no_file_change

### User

我想实现一个计算器,告诉我要怎么实现

## Case: chinese-calculator-no-code-design

expect: answer_only, chinese, no_file_change

### User

我想要你帮我用Python实现一个计算器.你不要写代码,先告诉我需要实现哪些功能

## Case: calculator-follow-up-implementation

expect: uses_context, tool_call, writes_calculator, chinese

### User

我想实现一个计算器,告诉我要怎么实现

### Assistant Context

可以实现一个 Python 命令行计算器，支持加减乘除、括号、异常处理和交互式输入。

### User

需要,帮我实现

## Case: calculator-shell-test

expect: tool_call, writes_calculator, shell_tool, shell_output_has_calculator_result, chinese

### User

帮我实现一个python计算器

### User

执行命令,测试一下这个计算器

## Case: hello-world-write

expect: tool_call, writes_hello_world

### User

写个hello world
