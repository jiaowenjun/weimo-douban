import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Input,
  Select,
  Space,
  Statistic,
  Table,
  Tag,
  theme,
  Tooltip,
  Typography,
} from 'antd';
import { PageContainer } from '@ant-design/pro-components';
import { CloudDownloadOutlined, RedoOutlined, ReloadOutlined, SearchOutlined } from '@ant-design/icons';
import dayjs from 'dayjs';
import type { Subject, SubjectListResponse } from '../types';
import { api, type ApiError } from '../api';
import { CATEGORY_OPTIONS } from '../labels';
import {
  SubjectCover,
  SubjectKeyFields,
  SubjectMetaDetail,
  SubjectRating,
} from '../subject-meta';

const PAGE_SIZE = 20;

/** 顶部进度卡：跨用户共享的富集进度与回填入口 */
function ProgressCard({
  counts,
  onBackfill,
  backfilling,
  onRequeueIntro,
  requeuingIntro,
}: {
  counts: SubjectListResponse['counts'] | null;
  onBackfill: () => void;
  backfilling: boolean;
  onRequeueIntro: () => void;
  requeuingIntro: boolean;
}) {
  return (
    <Card size="small" style={{ marginBottom: 16 }}>
      <Space size={32} wrap>
        <Statistic title="已富集" value={counts?.enriched ?? '—'} valueStyle={{ fontSize: 20 }} />
        <Statistic
          title="队列中"
          value={counts?.pending ?? '—'}
          valueStyle={{ fontSize: 20 }}
          prefix={counts && counts.pending > 0 ? <Tag color="processing">抓取中</Tag> : undefined}
        />
        <Statistic title="条目已删除" value={counts?.not_found ?? '—'} valueStyle={{ fontSize: 20 }} />
        <Statistic
          title="失败"
          value={counts?.failed ?? '—'}
          valueStyle={{ fontSize: 20 }}
          prefix={counts && counts.failed > 0 ? <Tag color="error">error</Tag> : undefined}
        />
        {(counts?.intro_fallback ?? 0) > 0 && (
          <Statistic
            title="intro 兜底"
            value={counts!.intro_fallback!}
            valueStyle={{ fontSize: 20 }}
            prefix={
              <Tooltip title="rexxar 重试耗尽后由 records.intro 合成的降纔回答（无豆瓣评分）；可单条「刷新」或「重排 intro 兜底」一键升级">
                <Tag color="gold">降级</Tag>
              </Tooltip>
            }
          />
        )}
        <Button icon={<CloudDownloadOutlined />} loading={backfilling} onClick={onBackfill}>
          回填存量
        </Button>
        {(counts?.intro_fallback ?? 0) > 0 && (
          <Tooltip title="把全部 source=intro 兜底条目无条件强制重排（优先重抓 rexxar，不受 TTL 限制），成功后升级回权威数据">
            <Button icon={<RedoOutlined />} loading={requeuingIntro} onClick={onRequeueIntro}>
              重排 intro 兜底
            </Button>
          </Tooltip>
        )}
      </Space>
    </Card>
  );
}

export default function Subjects() {
  const navigate = useNavigate();
  const { message } = AntApp.useApp();
  const { token } = theme.useToken();
  const [category, setCategory] = useState('all');
  const [q, setQ] = useState('');
  const [page, setPage] = useState(1);
  const [data, setData] = useState<SubjectListResponse | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [loading, setLoading] = useState(false);
  const [backfilling, setBackfilling] = useState(false);
  const [requeuingIntro, setRequeuingIntro] = useState(false);
  const [refreshing, setRefreshing] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setData(await api.listSubjects({ category, q, page, pageSize: PAGE_SIZE }));
      setError(null);
    } catch (e) {
      setData(null);
      setError(e as ApiError);
    } finally {
      setLoading(false);
    }
  }, [category, q, page]);

  useEffect(() => {
    load();
  }, [load]);

  const onBackfill = async () => {
    setBackfilling(true);
    try {
      const r = await api.backfillSubjects();
      if (r.queued_jobs > 0) {
        message.success(`已排入 ${r.queued_jobs} 个富集任务（覆盖 ${r.uid_category_pairs} 组用户×类别），worker 将在空闲时段抓取`);
        load();
      } else {
        message.info('没有需要回填的条目（全部为新鲜缓存）');
      }
    } catch (e) {
      message.error((e as ApiError).message);
    } finally {
      setBackfilling(false);
    }
  };

  const onRequeueIntro = async () => {
    setRequeuingIntro(true);
    try {
      const r = await api.requeueSubjectJobs('intro');
      if (r.queued > 0) {
        message.success(`已重排 ${r.queued} 个 intro 兜底条目，worker 将优先重抓升级`);
        load();
      } else {
        message.info('没有可重排的 intro 兜底条目（均已在队列中）');
      }
    } catch (e) {
      message.error((e as ApiError).message);
    } finally {
      setRequeuingIntro(false);
    }
  };

  const onRefresh = async (s: Subject) => {
    setRefreshing(s.subject_id);
    try {
      await api.refreshSubject(s.category, s.subject_id);
      message.success(`已重排「${s.title}」的富集任务，稍后刷新查看`);
    } catch (e) {
      message.error((e as ApiError).message);
    } finally {
      setRefreshing(null);
    }
  };

  return (
    <PageContainer
      title="条目元信息"
      subTitle="rexxar 结构化元数据缓存（跨用户共享，同一部作品只请求豆瓣一次）"
    >
      {error && (
        <Alert
          type={error.status === 404 ? 'info' : 'error'}
          showIcon
          style={{ marginBottom: 16 }}
          message={error.message}
          description={
            error.status === 404
              ? '服务器版本过旧或未启用富集（SYNC_SUBJECT_ENRICH）。重启新版服务后，点击「回填存量」导入已有标记的条目。'
              : undefined
          }
        />
      )}

      <ProgressCard
        counts={data?.counts ?? null}
        onBackfill={onBackfill}
        backfilling={backfilling}
        onRequeueIntro={onRequeueIntro}
        requeuingIntro={requeuingIntro}
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
            placeholder="按标题搜索"
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

      <Table<Subject>
        size="small"
        rowKey={(s) => `${s.category}/${s.subject_id}`}
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
        expandable={{
          expandedRowRender: (s) => <SubjectMetaDetail subject={s} />,
          rowExpandable: (s) => Boolean(s[s.category]),
        }}
        columns={[
          {
            title: '',
            dataIndex: 'cover',
            width: 52,
            render: (_, s) => <SubjectCover subject={s} size={36} />,
          },
          {
            title: '条目',
            dataIndex: 'title',
            render: (_, s) => (
              <>
                <div>
                  <a href={s.url} target="_blank" rel="noreferrer">
                    {s.title}
                  </a>{' '}
                  <Tag style={{ marginRight: 0 }}>{CATEGORY_OPTIONS.find((c) => c.value === s.category)?.label}</Tag>
                  {s.source === 'intro' && (
                    <Tooltip title="rexxar 重试耗尽的 intro 兜底数据（无豆瓣评分）；行末「刷新」或上方「重排 intro 兜底」可强制重抓升级">
                      <Tag color="gold" style={{ marginRight: 0 }}>
                        intro 兜底
                      </Tag>
                    </Tooltip>
                  )}
                </div>
                <SubjectKeyFields subject={s} />
              </>
            ),
          },
          {
            title: '年份',
            dataIndex: 'year',
            width: 70,
            render: (v) => v ?? '—',
          },
          {
            title: '豆瓣评分',
            dataIndex: 'rating',
            width: 130,
            render: (_, s) => <SubjectRating subject={s} />,
          },
          {
            title: '抓取于',
            dataIndex: 'fetched_at',
            width: 140,
            render: (v: string) => (
              <Tooltip title={v}>
                <span style={{ color: token.colorTextDescription }}>{dayjs(v).format('YYYY-MM-DD HH:mm')}</span>
              </Tooltip>
            ),
          },
          {
            title: '',
            key: 'actions',
            width: 70,
            render: (_, s) => (
              <Tooltip title="无视 TTL 重新抓取 rexxar">
                <Button
                  size="small"
                  type="text"
                  icon={<ReloadOutlined />}
                  loading={refreshing === s.subject_id}
                  onClick={() => onRefresh(s)}
                />
              </Tooltip>
            ),
          },
        ]}
      />
      {data && data.counts.pending > 0 && (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          队列中 {data.counts.pending} 个条目将在 worker 空闲时段抓取，可稍后点击「刷新」；可在
          <Typography.Link onClick={() => navigate('/subjects/jobs')}>富集任务</Typography.Link>
          页查看与管理队列
        </Typography.Text>
      )}
    </PageContainer>
  );
}
