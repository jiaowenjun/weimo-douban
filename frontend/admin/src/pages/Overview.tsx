import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Alert, AutoComplete, Badge, Button, Card, Col, InputNumber, Popconfirm, Row, Space, Statistic, Table, Tooltip, message } from 'antd';
import { PageContainer, ProCard } from '@ant-design/pro-components';
import { DeleteOutlined, PlusOutlined, ReloadOutlined } from '@ant-design/icons';
import dayjs from 'dayjs';
import type { ServerInfo, Subscription, TaskSummary } from '../types';
import { api, health, parseUserInput } from '../api';
import { TaskStatusTag } from '../labels';
import { UserCell } from '../user-profile';
import { useNoSpellCheck } from '../no-spellcheck';

const fmtInterval = (secs: number) =>
  secs % 86400 === 0 ? `${secs / 86400} 天` : secs % 3600 === 0 ? `${secs / 3600} 小时` : `${Math.round(secs / 60)} 分钟`;

const fmtTime = (v: string | null) =>
  v ? dayjs(v).format('MM-DD HH:mm') : '—';

export default function Overview() {
  const navigate = useNavigate();
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [online, setOnline] = useState<boolean | null>(null);
  const [connError, setConnError] = useState<string | null>(null);
  const [recent, setRecent] = useState<TaskSummary[]>([]);
  const [subs, setSubs] = useState<Subscription[]>([]);
  const [subUid, setSubUid] = useState('');
  const [subRss, setSubRss] = useState<number | null>(null);
  const [subFull, setSubFull] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);
  const [userOptions, setUserOptions] = useState<{ value: string; label: string }[]>([]);
  const [messageApi, contextHolder] = message.useMessage();
  // 订阅用户输入框收 ID/链接，macOS 上关掉自动拼写订正
  const subUidRef = useNoSpellCheck();

  const load = async () => {
    try {
      await health();
      setOnline(true);
      setConnError(null);
    } catch (e) {
      setOnline(false);
      setConnError((e as Error).message);
      return;
    }
    try {
      setInfo(await api.serverInfo());
    } catch {
      /* healthz 通而 API 报错时在卡片里显示空值即可 */
    }
    try {
      // 全后端视角：最近任务跨所有用户、含订阅触发的 auto 任务
      const res = await api.listTasks({ limit: 5, source: 'all' });
      setRecent(res.tasks);
    } catch {
      setRecent([]);
    }
    try {
      const res = await api.listSubscriptions();
      setSubs(res.subscriptions);
    } catch {
      setSubs([]);
    }
  };

  useEffect(() => {
    load();
    const refreshVisible = () => {
      if (document.visibilityState === 'visible') void load();
    };
    const timer = setInterval(refreshVisible, 30_000);
    document.addEventListener('visibilitychange', refreshVisible);
    return () => {
      clearInterval(timer);
      document.removeEventListener('visibilitychange', refreshVisible);
    };
  }, []);

  // 订阅对象自动补全：已知用户可选，也允许输入新 uid（旧版后端无 /users 时回退空）
  useEffect(() => {
    api
      .listUsers()
      .then((res) =>
        setUserOptions(
          res.users.map((u) => ({
            value: u.uid,
            label: u.username ? `${u.username}（@${u.uid}）` : `@${u.uid}`,
          })),
        ),
      )
      .catch(() => setUserOptions([]));
  }, []);

  const addSubscription = async () => {
    const uid = parseUserInput(subUid);
    if (!uid) {
      messageApi.warning('请选择已知用户，或输入豆瓣用户 ID / 个人主页链接');
      return;
    }
    setAdding(true);
    try {
      await api.createSubscription(uid, {
        ...(subRss != null ? { rss_interval_secs: subRss } : {}),
        ...(subFull != null ? { full_interval_secs: subFull } : {}),
      });
      messageApi.success(`已订阅 ${uid}（增量 10 分钟内生效，全量按周期自动刷新）`);
      setSubUid('');
      setSubRss(null);
      setSubFull(null);
      await load();
    } catch (e) {
      messageApi.error((e as Error).message);
    } finally {
      setAdding(false);
    }
  };

  const removeSubscription = async (uid: string) => {
    try {
      await api.deleteSubscription(uid);
      messageApi.success(`已取消订阅 ${uid}`);
      await load();
    } catch (e) {
      messageApi.error((e as Error).message);
    }
  };

  const circuitOpen = info?.crawl.circuit.open ?? false;
  const circuitRemainMin =
    info ? Math.ceil(info.crawl.circuit.remaining_ms / 60000) : 0;

  return (
    <PageContainer
      title="概览"
      subTitle={info ? `${info.name} v${info.version}` : undefined}
      extra={[
        <Button key="refresh" icon={<ReloadOutlined />} onClick={load}>
          刷新
        </Button>,
      ]}
    >
      {contextHolder}
      {connError && (
        <Alert
          type="error"
          showIcon
          style={{ marginBottom: 16 }}
          message="无法连接同步服务器"
          description={`请确认服务器已启动（默认 127.0.0.1:8080），或点击右上角「连接」修改 API 地址。错误：${connError}`}
        />
      )}
      {circuitOpen && (
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 16 }}
          message={`豆瓣抓取已熔断，预计 ${circuitRemainMin} 分钟后自动恢复。熔断期间新任务继续排队。`}
        />
      )}

      <Row gutter={[16, 16]}>
        <Col xs={24} sm={12} lg={6}>
          <Card size="small">
            <Statistic
              title="服务状态"
              valueRender={() =>
                online ? (
                  <Badge status="success" text="运行中" />
                ) : (
                  <Badge status="error" text="不可达" />
                )
              }
            />
          </Card>
        </Col>
        <Col xs={24} sm={12} lg={6}>
          <Card size="small">
            <Statistic title="队列（抓取中 / 排队）" value={info ? `${info.queue.running} / ${info.queue.queued}` : '—'} />
          </Card>
        </Col>
        <Col xs={24} sm={12} lg={6}>
          <Card size="small">
            <Statistic
              title="熔断器"
              valueRender={() =>
                info ? (
                  circuitOpen ? (
                    <Badge status="warning" text={`熔断中 · 剩余 ${circuitRemainMin} 分钟`} />
                  ) : (
                    <Badge status="success" text="正常" />
                  )
                ) : (
                  <>—</>
                )
              }
            />
          </Card>
        </Col>
        <Col xs={24} sm={12} lg={6}>
          <Card size="small">
            <Statistic
              title="鉴权"
              valueRender={() =>
                info ? (
                  info.auth_enabled ? (
                    <Badge status="processing" text="已启用 Token" />
                  ) : (
                    <Tooltip title="公网部署建议设置 EXPORT_SERVER_TOKEN">
                      <Badge status="default" text="未启用" />
                    </Tooltip>
                  )
                ) : (
                  <>—</>
                )
              }
            />
          </Card>
        </Col>
      </Row>

      <ProCard title="运行参数" size="small" style={{ marginTop: 16 }} headerBordered>
        {info ? (
          <div>
            {info.cookie_enabled ? '登录抓取（Cookie 已配置，可取隐藏条目）' : '免登录抓取（豆瓣隐藏条目不可见）'} · 抓取间隔 ≥{' '}
            {info.crawl.min_interval_seconds}s · 缓存 TTL {Math.round(info.cache.ttl_seconds / 3600)} 小时
            · 任务保留 {info.cache.retention_task_days} 天 · 熔断阈值 {info.crawl.circuit.threshold} 次 / 冷却 {Math.round(info.crawl.circuit.cooldown_seconds / 60)} 分钟</div>
        ) : (
          <div>—</div>
        )}
      </ProCard>

      <ProCard
        title="订阅同步"
        size="small"
        style={{ marginTop: 16 }}
        headerBordered
        extra={
          <Tooltip title="订阅用户由后端自动同步：短周期抓取豆瓣 RSS 增量补新标记，长周期自动发起全量任务刷新全部维度；未订阅用户仍手动建任务同步">
            <span style={{ cursor: 'help' }}>RSS 增量 + 定时全量</span>
          </Tooltip>
        }
      >
        <Space wrap style={{ marginBottom: 12 }}>
          <AutoComplete
            ref={subUidRef}
            value={subUid}
            onChange={setSubUid}
            options={userOptions}
            style={{ width: 240 }}
            filterOption={(input, option) =>
              (option?.value ?? '').toLowerCase().includes(input.toLowerCase()) ||
              (option?.label ?? '').toLowerCase().includes(input.toLowerCase())
            }
            placeholder="订阅用户：选择已知用户或输入 ID / 主页链接"
          />
          <InputNumber
            placeholder="增量间隔（秒，默认 3600）"
            style={{ width: 180 }}
            min={60}
            value={subRss}
            onChange={setSubRss}
          />
          <InputNumber
            placeholder="全量间隔（秒，默认 86400）"
            style={{ width: 190 }}
            min={3600}
            value={subFull}
            onChange={setSubFull}
          />
          <Button type="primary" icon={<PlusOutlined />} loading={adding} onClick={addSubscription}>
            订阅
          </Button>
        </Space>
        <Table<Subscription>
          size="small"
          rowKey="uid"
          pagination={false}
          dataSource={subs}
          locale={{ emptyText: '暂无订阅——订阅后该用户的标记变更将自动同步入库' }}
          columns={[
            {
              title: '用户',
              dataIndex: 'uid',
              width: 180,
              render: (_, r) => (
                <Space size={4}>
                  <UserCell uid={r.uid} />
                  {!r.enabled && <Badge status="default" text="停用" />}
                </Space>
              ),
            },
            {
              title: '增量 / 全量',
              width: 150,
              render: (_, r) => (
                <span>
                  {fmtInterval(r.rss_interval_secs)} / {fmtInterval(r.full_interval_secs)}
                </span>
              ),
            },
            { title: '下次增量', dataIndex: 'next_rss_at', width: 110, render: (_, r) => fmtTime(r.next_rss_at) },
            { title: '下次全量', dataIndex: 'next_full_at', width: 110, render: (_, r) => fmtTime(r.next_full_at) },
            { title: '上次增量', dataIndex: 'last_rss_at', width: 110, render: (_, r) => fmtTime(r.last_rss_at) },
            { title: '上次全量', dataIndex: 'last_full_at', width: 110, render: (_, r) => fmtTime(r.last_full_at) },
            {
              title: '缓存',
              width: 100,
              render: (_, r) => `${r.dimensions} 维 / ${r.records} 条`,
            },
            {
              title: '状态',
              ellipsis: true,
              render: (_, r) =>
                r.last_error ? (
                  <Tooltip title={r.last_error}>
                    <Badge status="error" text={r.last_error} />
                  </Tooltip>
                ) : (
                  <Badge status="success" text="正常" />
                ),
            },
            {
              title: '操作',
              width: 70,
              render: (_, r) => (
                <Popconfirm title={`取消订阅 ${r.uid}？`} onConfirm={() => removeSubscription(r.uid)}>
                  <Button type="link" size="small" danger icon={<DeleteOutlined />} />
                </Popconfirm>
              ),
            },
          ]}
        />
      </ProCard>

      <ProCard
        title="最近任务"
        size="small"
        style={{ marginTop: 16 }}
        headerBordered
        extra={
          <Button type="link" size="small" onClick={() => navigate('/tasks')}>
            查看全部
          </Button>
        }
      >
        <Table<TaskSummary>
          size="small"
          rowKey="task_id"
          pagination={false}
          dataSource={recent}
          onRow={(r) => ({ onClick: () => navigate(`/tasks?task=${r.task_id}`), style: { cursor: 'pointer' } })}
          columns={[
            { title: '任务 ID', dataIndex: 'task_id', ellipsis: true },
            {
              title: '用户',
              dataIndex: 'user',
              width: 140,
              ellipsis: true,
              render: (_, r) => <UserCell uid={r.user} />,
            },
            { title: '状态', dataIndex: 'status', width: 90, render: (_, r) => <TaskStatusTag status={r.status} /> },
            { title: '来源', dataIndex: 'source', width: 70, render: (_, r) => (r.source === 'auto' ? '订阅' : '手动') },
            { title: '条目', dataIndex: 'items_count', width: 70 },
            {
              title: '创建时间',
              dataIndex: 'created_at',
              width: 150,
              render: (_, r) => dayjs(r.created_at).format('MM-DD HH:mm:ss'),
            },
          ]}
        />
      </ProCard>
    </PageContainer>
  );
}
