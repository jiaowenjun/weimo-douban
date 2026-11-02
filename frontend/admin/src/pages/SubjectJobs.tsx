import { useCallback, useEffect, useState } from 'react';
import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Input,
  Popconfirm,
  Select,
  Space,
  Statistic,
  Table,
  Tabs,
  Tag,
  Tooltip,
  Typography,
  theme,
} from 'antd';
import { PageContainer } from '@ant-design/pro-components';
import {
  CloudUploadOutlined,
  DeleteOutlined,
  PauseCircleOutlined,
  ReloadOutlined,
  SearchOutlined,
} from '@ant-design/icons';
import dayjs from 'dayjs';
import type {
  EnrichmentOverview as Overview,
  SubjectJob,
  SubjectJobListResponse,
  SubjectJobState,
} from '../types';
import { api, resolveApiUrl, type ApiError } from '../api';
import { CATEGORY_OPTIONS } from '../labels';
import EnrichmentOverview from './EnrichmentOverview';

const PAGE_SIZE = 20;

const STATE_TABS: { key: string; label: string; state?: string }[] = [
  { key: 'all', label: '全部' },
  { key: 'pending', label: '待执行', state: 'queued,running' },
  { key: 'ok', label: '已完成', state: 'ok' },
  { key: 'error', label: '失败', state: 'error' },
  { key: 'not_found', label: '条目已删除', state: 'not_found' },
];

function stateTag(s: SubjectJobState) {
  switch (s) {
    case 'queued':
      return <Tag color="blue">排队中</Tag>;
    case 'running':
      return <Tag color="processing">抓取中</Tag>;
    case 'ok':
      return <Tag color="success">已完成</Tag>;
    case 'error':
      return <Tag color="error">失败</Tag>;
    default:
      return <Tag>条目已删除</Tag>;
  }
}

/** 顶部统计卡：各状态计数 + 中断/重排/清理操作 */
function StatsCard({
  counts,
  busy,
  onInterrupt,
  onRequeue,
  onClearOk,
  retrying,
}: {
  counts: SubjectJobListResponse['counts'] | null;
  busy: string | null;
  retrying: number;
  onInterrupt: () => void;
  onRequeue: (scope: 'failed' | 'not_found' | 'stale') => void;
  onClearOk: () => void;
}) {
  const pending = counts ? counts.queued + counts.running : 0;
  return (
    <Card size="small" style={{ marginBottom: 16 }}>
      <Space size={32} wrap>
        <Statistic
          title="待执行"
          value={pending}
          valueStyle={{ fontSize: 20 }}
          prefix={retrying > 0 ? <Tag color="gold">含 {retrying} 条重试</Tag> : undefined}
        />
        <Statistic title="已完成" value={counts?.ok ?? '—'} valueStyle={{ fontSize: 20 }} />
        <Statistic
          title="失败"
          value={counts?.error ?? '—'}
          valueStyle={{ fontSize: 20 }}
          prefix={counts && counts.error > 0 ? <Tag color="error">error</Tag> : undefined}
        />
        <Statistic
          title="条目已删除"
          value={counts?.not_found ?? '—'}
          valueStyle={{ fontSize: 20 }}
        />
        <Popconfirm
          title="清空全部待执行任务"
          description={`当前共 ${pending} 条，其中 ${retrying} 条等待重试。删除前保存快照，可在操作记录中恢复；在途请求允许结束。如需暂时停止，请使用暂停。`}
          okText="保存快照并清空"
          okButtonProps={{ danger: true }}
          onConfirm={onInterrupt}
          disabled={pending === 0}
        >
          <Button
            danger
            icon={<PauseCircleOutlined />}
            disabled={pending === 0}
            loading={busy === 'interrupt'}
          >
            清空待执行
          </Button>
        </Popconfirm>
        <Tooltip title="重试全部失败终态的任务（error）">
          <Button
            icon={<CloudUploadOutlined />}
            disabled={!counts || counts.error === 0}
            loading={busy === 'failed'}
            onClick={() => onRequeue('failed')}
          >
            重排失败{counts?.error ? ` (${counts.error})` : ''}
          </Button>
        </Tooltip>
        <Tooltip title="豆瓣已删除的条目（404 终态）通常无需重试，仅在怀疑误判时使用">
          <Button
            disabled={!counts || counts.not_found === 0}
            loading={busy === 'not_found'}
            onClick={() => onRequeue('not_found')}
          >
            重排已删除{counts?.not_found ? ` (${counts.not_found})` : ''}
          </Button>
        </Tooltip>
        <Tooltip title="强制重抓库中已过 TTL 的条目——「重新抓取已有条目」的唯一批量入口">
          <Button
            icon={<ReloadOutlined />}
            loading={busy === 'stale'}
            onClick={() => onRequeue('stale')}
          >
            重抓过期
          </Button>
        </Tooltip>
        <Popconfirm
          title="清理已完成的任务记录"
          description="仅删除 job 记录，subjects 缓存数据保留"
          okText="清理"
          onConfirm={onClearOk}
          disabled={!counts || counts.ok === 0}
        >
          <Button disabled={!counts || counts.ok === 0} loading={busy === 'clearOk'}>
            清理已完成{counts?.ok ? ` (${counts.ok})` : ''}
          </Button>
        </Popconfirm>
      </Space>
    </Card>
  );
}

export default function SubjectJobs() {
  const { message, modal } = AntApp.useApp();
  const { token } = theme.useToken();
  const [tab, setTab] = useState('all');
  const [category, setCategory] = useState('all');
  const [q, setQ] = useState('');
  const [page, setPage] = useState(1);
  const [data, setData] = useState<SubjectJobListResponse | null>(null);
  const [overview, setOverview] = useState<Overview | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [acting, setActing] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const state = STATE_TABS.find((t) => t.key === tab)?.state;
      const [jobs, summary] = await Promise.all([
        api.listSubjectJobs({ state, category, q, page, pageSize: PAGE_SIZE }),
        api.enrichmentOverview(),
      ]);
      setData(jobs);
      setOverview(summary);
      setError(null);
    } catch (e) {
      setData(null);
      setError(e as ApiError);
    } finally {
      setLoading(false);
    }
  }, [tab, category, q, page]);

  useEffect(() => {
    load();
  }, [load]);

  // 队列有活儿时自动轮询（页面不可见时跳过）
  useEffect(() => {
    const pending = data ? data.counts.queued + data.counts.running : 0;
    if (pending <= 0) return;
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') load();
    }, 5000);
    return () => clearInterval(timer);
  }, [data, load]);

  const after = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    try {
      await fn();
      await load();
    } catch (e) {
      message.error((e as ApiError).message);
    } finally {
      setBusy(null);
    }
  };

  const onControl = (action: 'pause' | 'resume' | 'restore' | 'backfill', value?: string) =>
    after(action, async () => {
      if (action === 'restore') {
        const r = await api.restoreEnrichment(value!);
        message.success(
          `已恢复 ${r.restored} 条任务${r.in_flight ? `；${r.in_flight} 条仍在执行，可稍后再恢复` : ''}`,
        );
      } else if (action === 'backfill') {
        const r = await api.backfillSubjects();
        message.success(`已补回 ${r.queued_jobs} 条缺失任务`);
      } else {
        const r = await api.controlEnrichment(action === 'pause', value);
        message.success(
          action === 'pause'
            ? `已暂停；${r.in_flight} 条在途请求允许结束`
            : '已恢复，仍遵守类别冷却',
        );
      }
    });

  const onInterrupt = () =>
    after('interrupt', async () => {
      const r = await api.clearSubjectJobs({ states: ['queued', 'running'], category: 'all' });
      message.success(
        `已保存快照并删除 ${r.deleted} 条，其中 ${r.retrying} 条等待重试；${r.in_flight} 条在途请求允许结束`,
      );
    });

  const onRequeue = (scope: 'failed' | 'not_found' | 'stale') =>
    after(scope, async () => {
      const r = await api.requeueSubjectJobs(scope);
      if (r.queued > 0) {
        message.success(`已重排 ${r.queued} 个任务（优先执行）`);
      } else {
        message.info('没有符合条件的任务');
      }
    });

  const onClearOk = () =>
    after('clearOk', async () => {
      const r = await api.clearSubjectJobs({ states: ['ok'] });
      message.success(`已清理 ${r.deleted} 条完成记录`);
    });

  const onForce = async (j: SubjectJob) => {
    setActing(j.subject_id);
    try {
      await api.refreshSubject(j.category, j.subject_id);
      message.success(`已强制重排「${j.title ?? j.subject_id}」，worker 将优先抓取`);
      await load();
    } catch (e) {
      message.error((e as ApiError).message);
    } finally {
      setActing(null);
    }
  };

  const onDelete = (j: SubjectJob) => {
    modal.confirm({
      title: `删除任务「${j.title ?? j.subject_id}」？`,
      content:
        j.state === 'queued' || j.state === 'running'
          ? '将取消该条目的富集（subjects 已有数据不受影响）'
          : '仅删除任务记录；失败/已删除条目在下次同步后可重新入队',
      okText: '删除',
      okButtonProps: { danger: true },
      onOk: async () => {
        await api.deleteSubjectJob(j.category, j.subject_id);
        message.success('已删除');
        await load();
      },
    });
  };

  const counts = data?.counts ?? null;

  return (
    <PageContainer
      title="富集任务"
      subTitle="结构化元信息富集与任务管理"
    >
      {error && (
        <Alert
          type={error.status === 404 ? 'info' : 'error'}
          showIcon
          style={{ marginBottom: 16 }}
          message={error.message}
          description={
            error.status === 404
              ? '服务器版本过旧，不支持富集任务管理端点。升级后端后重试。'
              : undefined
          }
        />
      )}

      <EnrichmentOverview data={overview} busy={busy !== null} act={onControl} />
      <StatsCard
        counts={counts}
        retrying={overview?.categories.reduce((n, c) => n + c.retry_waiting, 0) ?? 0}
        busy={busy}
        onInterrupt={onInterrupt}
        onRequeue={onRequeue}
        onClearOk={onClearOk}
      />

      <Card size="small" style={{ marginBottom: 16 }}>
        <Space>
          <Select
            value={category}
            onChange={(v) => {
              setPage(1);
              setCategory(v);
            }}
            style={{ width: 100 }}
            options={[{ value: 'all', label: '全部' }, ...CATEGORY_OPTIONS]}
          />
          <Input.Search
            placeholder="按标题或条目 ID 搜索"
            allowClear
            style={{ width: 240 }}
            prefix={<SearchOutlined />}
            onSearch={(v) => {
              setPage(1);
              setQ(v.trim());
            }}
          />
          <Button icon={<ReloadOutlined />} onClick={load}>
            刷新
          </Button>
        </Space>
      </Card>

      <Tabs
        activeKey={tab}
        onChange={(v) => {
          setPage(1);
          setTab(v);
        }}
        items={STATE_TABS.map((t) => {
          const n =
            t.key === 'all'
              ? data?.counts_total
              : t.key === 'pending'
                ? counts
                  ? counts.queued + counts.running
                  : undefined
                : counts?.[t.key as SubjectJobState];
          return { key: t.key, label: n != null ? `${t.label} (${n})` : t.label };
        })}
        style={{ marginBottom: 8 }}
      />

      <Table<SubjectJob>
        size="small"
        scroll={{ x: 1200 }}
        rowKey={(j) => `${j.category}/${j.subject_id}`}
        loading={loading}
        dataSource={data?.items ?? []}
        pagination={
          data
            ? {
                current: page,
                pageSize: PAGE_SIZE,
                total: data.total,
                showSizeChanger: false,
                onChange: setPage,
              }
            : false
        }
        columns={[
          {
            title: '',
            dataIndex: 'cover',
            width: 52,
            render: (_, j) =>
              j.cover ? (
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={resolveApiUrl(j.cover)}
                  alt={j.title ?? j.subject_id}
                  width={36}
                  height={50}
                  style={{ objectFit: 'cover', borderRadius: 4, display: 'block' }}
                />
              ) : null,
          },
          {
            title: '条目',
            dataIndex: 'title',
            width: 260,
            render: (_, j) => (
              <>
                <div>
                  {j.title ?? <Typography.Text type="secondary">（未富集）</Typography.Text>}{' '}
                  <Tag style={{ marginRight: 0 }}>
                    {CATEGORY_OPTIONS.find((c) => c.value === j.category)?.label}
                  </Tag>{' '}
                  {j.forced && <Tag color="orange">强制</Tag>}
                  {j.stale && <Tag color="gold">数据过期</Tag>}
                </div>
                <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                  {j.subject_id}
                </Typography.Text>
              </>
            ),
          },
          {
            title: '状态',
            dataIndex: 'state',
            width: 100,
            render: (s: SubjectJobState, j) => {
              const policy = overview?.categories.find((c) => c.category === j.category);
              if (s === 'queued' && policy?.paused)
                return <Tag>已暂停{j.attempts > 0 ? ' · 待重试' : ''}</Tag>;
              if (s === 'queued' && j.attempts > 0) return <Tag color="gold">等待重试</Tag>;
              if (s === 'queued' && policy && policy.blocked_until_ms > overview!.now_ms)
                return <Tag color="gold">类别冷却</Tag>;
              return stateTag(s);
            },
          },
          {
            title: '尝试',
            dataIndex: 'attempts',
            width: 60,
            render: (v: number) => (v > 0 ? v : '—'),
          },
          {
            title: '下次执行 / 抓取于',
            width: 150,
            render: (_, j) => {
              const policy = overview?.categories.find((c) => c.category === j.category);
              const v =
                j.state === 'queued' || j.state === 'running'
                  ? Math.max(
                      j.next_run_at ? dayjs(j.next_run_at).valueOf() : 0,
                      policy?.next_request_ms ?? 0,
                    )
                  : j.fetched_at;
              return v ? (
                <span style={{ color: token.colorTextDescription }}>
                  {dayjs(v).format('YYYY-MM-DD HH:mm')}
                </span>
              ) : (
                '—'
              );
            },
          },
          {
            title: '更新于',
            dataIndex: 'updated_at',
            width: 140,
            render: (v: string) => (
              <span style={{ color: token.colorTextDescription }}>
                {dayjs(v).format('YYYY-MM-DD HH:mm:ss')}
              </span>
            ),
          },
          {
            title: '错误',
            dataIndex: 'last_error',
            ellipsis: true,
            render: (v: string | null, j) =>
              v ? (
                <Tooltip title={v}>
                  <Typography.Text
                    type={j.state === 'queued' || j.state === 'running' ? 'warning' : 'danger'}
                    style={{ fontSize: 12 }}
                    ellipsis
                  >
                    {v}
                  </Typography.Text>
                </Tooltip>
              ) : (
                '—'
              ),
          },
          {
            title: '',
            key: 'actions',
            width: 90,
            render: (_, j) => (
              <Space size={0}>
                <Tooltip title="强制重排（插队执行；已有缓存也会重抓）">
                  <Button
                    size="small"
                    type="text"
                    icon={<ReloadOutlined />}
                    loading={acting === j.subject_id}
                    onClick={() => onForce(j)}
                  />
                </Tooltip>
                <Tooltip title="删除任务">
                  <Button
                    size="small"
                    type="text"
                    danger
                    icon={<DeleteOutlined />}
                    onClick={() => onDelete(j)}
                  />
                </Tooltip>
              </Space>
            ),
          },
        ]}
      />
      {data && data.counts.queued + data.counts.running > 0 && (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          队列每 5 秒自动刷新；暂停保留进度，清空保存快照。恢复和强制重排均遵守暂停与类别冷却。
        </Typography.Text>
      )}
    </PageContainer>
  );
}
