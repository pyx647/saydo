# SayDo 启动入口（双击或开机自启用 pythonw 运行，无控制台窗口）
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from app.main import main

main()
