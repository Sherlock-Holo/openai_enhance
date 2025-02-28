# openai_enhance

openai enhance proxy server

## Features

- extract Deepseek style CoT to `reasoning_content`
- truncate input token to specify max token size

## Usage

```bash
Usage: openai_enhance [OPTIONS] --listen <LISTEN> --backend <BACKEND>

Options:
  -l, --listen <LISTEN>                    listen addr
  -b, --backend <BACKEND>                  backend addr
  -i, --input-max-token <INPUT_MAX_TOKEN>  limit input token size
      --cot-parser <COT_PARSER>            [possible values: deepseek]
  -d, --debug                              enable debug log
  -h, --help                               Print help
```
