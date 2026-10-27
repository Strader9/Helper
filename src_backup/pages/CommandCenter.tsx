import { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import {
  MessageSquare, Cpu, Wrench, Users, Brain, Palette,
  Volume2, Music, Settings, Globe, Activity, Bot,
  Mic, MicOff, Zap,
} from 'lucide-react';
import { EAssistantCoreSphere, type EAssistantCoreOrbitTask } from '../components/EAssistantCoreSphere';
import '../styles/eassistant-command-center.css';

// 左侧功能入口
const APP_ENTRIES = [
  { icon: Activity, label: '运行日志', color: '#06b6d4', route: '/logs' },
  { icon: Wrench, label: '工具', color: '#f97316', route: '/tools' },
  { icon: Cpu, label: '技能大厅', color: '#22c55e', route: '/skills' },
  { icon: Users, label: 'Team', color: '#3b82f6', route: '/tasks' },
  { icon: Brain, label: '记忆化身', color: '#a855f7', route: '/memory' },
  { icon: Palette, label: '形象设计室', color: '#0ea5e9', route: '/media' },
  { icon: Volume2, label: '声音', color: '#6366f1', route: '/settings' },
  { icon: Music, label: 'Music', color: '#ef4444', route: '/media' },
];

// 人格卡片
const PERSONAS = [
  {
    name: '苍穹',
    role: '聊天',
    permission: '请求批准',
    color: 'linear-gradient(135deg, #f59e0b, #d97706)',
    icon: MessageSquare,
    level: 30,
  },
  {
    name: '星云',
    role: '助手',
    permission: '替我审批',
    color: 'linear-gradient(135deg, #a855f7, #7c3aed)',
    icon: Bot,
    level: 60,
  },
  {
    name: '赛博',
    role: '自主',
    permission: '完全访问',
    color: 'linear-gradient(135deg, #22c55e, #16a34a)',
    icon: Zap,
    level: 100,
  },
];

// 底部导航
const NAV_ITEMS = [
  { icon: Globe, label: '主页', route: '/' },
  { icon: Cpu, label: '核心', route: '/chat' },
  { icon: MessageSquare, label: '对话', route: '/chat' },
  { icon: Users, label: '团队', route: '/tasks' },
  { icon: Activity, label: '活动', route: '/logs' },
  { icon: Settings, label: '设置', route: '/settings' },
];

export function CommandCenter() {
  const navigate = useNavigate();
  const [activePersona, setActivePersona] = useState(1);
  const [voiceActive, setVoiceActive] = useState(false);
  const [activeNav, setActiveNav] = useState(0);

  // 模拟任务轨道（后续接入真实任务状态）
  const tasks: EAssistantCoreOrbitTask[] = [
    { id: '1', title: '系统监控', state: 'working', active: true },
    { id: '2', title: '记忆整理', state: 'queued', active: true },
  ];

  const handleNavClick = (index: number, route: string) => {
    setActiveNav(index);
    navigate(route);
  };

  return (
    <div className="command-center-root">
      {/* 中央粒子球指挥场 */}
      <EAssistantCoreSphere
        tasks={tasks}
        state="ready"
        labels={{
          aria: 'E-Assisant 指挥中心',
          eAssistantCore: 'E-ASSISANT',
          ready: '待命',
          working: '执行中',
          attention: '需关注',
          noTasks: '暂无活跃任务',
        }}
      />

      {/* 左侧功能入口 */}
      <div className="command-center-left-panel">
        {APP_ENTRIES.map((entry, index) => (
          <div
            key={index}
            className="cc-app-icon"
            onClick={() => navigate(entry.route)}
          >
            <div
              className="cc-app-icon-box"
              style={{ background: `linear-gradient(135deg, ${entry.color}, ${entry.color}cc)` }}
            >
              <entry.icon size={22} />
            </div>
            <span className="cc-app-icon-label">{entry.label}</span>
          </div>
        ))}
      </div>

      {/* 右侧人格卡片 + 学习流 */}
      <div className="command-center-right-panel">
        {PERSONAS.map((persona, index) => (
          <div
            key={index}
            className="cc-persona-card"
            style={{
              background: persona.color,
              opacity: activePersona === index ? 1 : 0.7,
              transform: activePersona === index ? 'scale(1.02)' : 'scale(1)',
            }}
            onClick={() => setActivePersona(index)}
          >
            <div className="cc-persona-card-header">
              <persona.icon size={18} />
              <span className="cc-persona-card-name">{persona.name}</span>
              <span className="cc-persona-card-role">· {persona.role}</span>
            </div>
            <div className="cc-persona-card-perm">{persona.permission}</div>
            <div className="cc-persona-card-bar">
              <div
                className="cc-persona-card-bar-fill"
                style={{ width: `${persona.level}%` }}
              />
            </div>
          </div>
        ))}

        {/* 学习流面板 */}
        <div className="cc-learning-panel">
          <div className="cc-learning-panel-title">
            <span className="cc-learning-panel-title-text">GUARDIAN 学习流</span>
            <span className="cc-learning-panel-badge">实时</span>
          </div>
          <div className="cc-learning-panel-body">
            正在学习你的使用习惯...
            <br />
            已记录 128 条交互模式
            <br />
            今日优化 3 项工具调用
          </div>
        </div>
      </div>

      {/* 底部语音交互区 */}
      <div className="command-center-voice-area">
        <div
          className="cc-voice-button"
          onClick={() => setVoiceActive(!voiceActive)}
          style={{
            background: voiceActive ? 'rgba(239, 68, 68, .2)' : 'rgba(255,255,255,.08)',
            borderColor: voiceActive ? 'rgba(239, 68, 68, .5)' : 'rgba(255,255,255,.15)',
            color: voiceActive ? '#ef4444' : 'rgba(255,255,255,.7)',
          }}
        >
          {voiceActive ? <MicOff size={24} /> : <Mic size={24} />}
        </div>
        <span className="cc-voice-hint">
          {voiceActive ? '正在监听...' : '语音交互 点击开始语音会话'}
        </span>
      </div>

      {/* 底部图标导航 */}
      <div className="command-center-bottom-bar">
        {NAV_ITEMS.map((item, index) => (
          <div
            key={index}
            className={`cc-nav-item ${activeNav === index ? 'active' : ''}`}
            onClick={() => handleNavClick(index, item.route)}
            title={item.label}
          >
            <item.icon size={20} />
          </div>
        ))}
      </div>
    </div>
  );
}
